use crate::engine::plan::ResponsePlan;
use crate::engine::scenario::{SuccessScript, TranscriptEvent};
use async_stream::stream;
use axum::body::Body;
use axum::http::{header, Response};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};

pub fn messages_events(plan: &ResponsePlan) -> Vec<TranscriptEvent> {
    let mut events = Vec::new();
    let mut start = plan.messages_json();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["stop_sequence"] = Value::Null;
    start["usage"]["output_tokens"] = json!(0);
    start
        .as_object_mut()
        .expect("message object")
        .remove("stop_details");
    push(&mut events, "message_start", json!({"message":start}));
    for (index, block) in plan.content.iter().enumerate() {
        let mut initial = block.clone();
        let kind = block["type"].as_str().unwrap_or_default();
        match kind {
            "text" => initial["text"] = json!(""),
            "thinking" => {
                initial["thinking"] = json!("");
                initial
                    .as_object_mut()
                    .expect("thinking block")
                    .remove("signature");
            }
            "tool_use" | "server_tool_use" => initial["input"] = json!({}),
            _ => {}
        }
        push(
            &mut events,
            "content_block_start",
            json!({"index":index,"content_block":initial}),
        );
        let delta = match kind {
            "text" => Some(json!({"type":"text_delta","text":block["text"]})),
            "thinking" => Some(json!({"type":"thinking_delta","thinking":block["thinking"]})),
            "tool_use" | "server_tool_use" => Some(
                json!({"type":"input_json_delta","partial_json":plan.raw_arguments.get(&index).cloned().unwrap_or_else(|| block["input"].to_string())}),
            ),
            _ => None,
        };
        if let Some(delta) = delta {
            push(
                &mut events,
                "content_block_delta",
                json!({"index":index,"delta":delta}),
            );
        }
        if kind == "thinking" {
            push(
                &mut events,
                "content_block_delta",
                json!({"index":index,"delta":{"type":"signature_delta","signature":block["signature"]}}),
            );
        }
        push(&mut events, "content_block_stop", json!({"index":index}));
    }
    let mut delta = json!({"stop_reason":plan.stop_reason,"stop_sequence":plan.stop_sequence});
    if let Some(details) = &plan.stop_details {
        delta["stop_details"] = details.clone();
    }
    push(
        &mut events,
        "message_delta",
        json!({"delta":delta,"usage":{"output_tokens":plan.usage.output_tokens}}),
    );
    push(&mut events, "message_stop", json!({}));
    events
}

fn push(events: &mut Vec<TranscriptEvent>, name: &str, mut data: Value) {
    data["type"] = json!(name);
    events.push(TranscriptEvent {
        event: Some(name.to_owned()),
        data: data.to_string(),
    });
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

pub fn event_response(events: Vec<TranscriptEvent>, options: &SuccessScript) -> Response<Body> {
    let close_after_chunks = options.close_after_chunks;
    let inter_event_delay_ms = options.inter_event_delay_ms;
    let malformed_sse = options.malformed_sse;
    let body = Body::from_stream(stream! {
        for (index,event) in events.into_iter().enumerate() {
            if close_after_chunks.is_some_and(|limit| index >= limit) { return; }
            if index > 0 && inter_event_delay_ms > 0 { sleep(Duration::from_millis(inter_event_delay_ms)).await; }
            // Dispatch the fault before terminal success. Consumers may stop
            // reading as soon as they see message_stop (or its stop reason).
            if malformed_sse && matches!(event.event.as_deref(), Some("message_delta" | "message_stop")) {
                yield Ok::<_,std::io::Error>("event: content_block_delta\ndata: {broken\n\n".to_owned());
                return;
            }
            yield Ok::<_,std::io::Error>(render_event(&event));
        }
    });
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response
}
