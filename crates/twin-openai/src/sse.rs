use async_stream::stream;
use axum::body::Body;
use axum::http::{header, HeaderValue, Response, StatusCode};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};

use crate::engine::failures::{TranscriptBody, TranscriptOutcome, TransportOptions};
use crate::engine::plan::ResponsePlan;
use crate::engine::scenario::TranscriptEvent;

pub fn responses_sse_response(plan: &ResponsePlan, transport: TransportOptions) -> Response<Body> {
    let mut events = Vec::new();
    let reasoning_item_id = format!("rs_{}", plan.id);
    let message_item_id = format!("msg_{}", plan.id);
    let mut next_output_index = 0;
    let streamed_text = plan.structured_output.as_ref().map(Value::to_string);

    events.push(sse_event(
        "response.created",
        &json!({
            "type": "response.created",
            "response": {
                "id": plan.id,
                "object": "response",
                "created": plan.created,
                "model": plan.model,
                "status": "in_progress",
                "output": [],
            },
        }),
    ));
    events.push(sse_event(
        "response.in_progress",
        &json!({
            "type": "response.in_progress",
            "response": {
                "id": plan.id,
                "object": "response",
                "created": plan.created,
                "model": plan.model,
                "status": "in_progress",
                "output": [],
            },
        }),
    ));

    events.push(sse_event(
        "response.output_item.added",
        &json!({
            "type": "response.output_item.added",
            "item": {
                "id": reasoning_item_id,
                "type": "reasoning",
                "summary": [],
            },
            "output_index": next_output_index,
        }),
    ));
    for reasoning in &plan.reasoning {
        events.push(sse_event(
            "response.reasoning.delta",
            &json!({
                "type": "response.reasoning.delta",
                "delta": reasoning,
                "item_id": reasoning_item_id,
                "output_index": next_output_index,
            }),
        ));
    }
    events.push(sse_event(
        "response.output_item.done",
        &json!({
            "type": "response.output_item.done",
            "item": {
                "id": reasoning_item_id,
                "type": "reasoning",
                "summary": [],
            },
            "output_index": next_output_index,
        }),
    ));
    next_output_index += 1;

    if !plan.response_text.is_empty() || streamed_text.is_some() {
        events.push(sse_event(
            "response.output_item.added",
            &json!({
                "type": "response.output_item.added",
                "item": {
                    "id": message_item_id,
                    "type": "message",
                    "status": "in_progress",
                    "content": [],
                    "role": "assistant",
                },
                "output_index": next_output_index,
            }),
        ));

        let message_text = streamed_text
            .as_deref()
            .unwrap_or(plan.response_text.as_str());

        if !message_text.is_empty() {
            events.push(sse_event(
                "response.content_part.added",
                &json!({
                    "type": "response.content_part.added",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "part": {
                        "type": "output_text",
                        "text": "",
                    },
                }),
            ));
            events.push(sse_event(
                "response.output_text.delta",
                &json!({
                    "type": "response.output_text.delta",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "delta": message_text,
                }),
            ));
            events.push(sse_event(
                "response.output_text.done",
                &json!({
                    "type": "response.output_text.done",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "text": message_text,
                }),
            ));
            events.push(sse_event(
                "response.content_part.done",
                &json!({
                    "type": "response.content_part.done",
                    "content_index": 0,
                    "item_id": message_item_id,
                    "output_index": next_output_index,
                    "part": {
                        "type": "output_text",
                        "text": message_text,
                    },
                }),
            ));
        }

        // The completed item carries its full content, like the real API.
        // Adapters round-trip this item verbatim into the next request's
        // input, so omitting content here produces an invalid replay.
        events.push(sse_event(
            "response.output_item.done",
            &json!({
                "type": "response.output_item.done",
                "item": {
                    "id": message_item_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": message_text,
                    }],
                },
                "output_index": next_output_index,
            }),
        ));
        next_output_index += 1;
    }

    for tool_call in &plan.tool_calls {
        // The added item carries an empty payload; the done item carries the
        // whole call. Between them the payload streams in one delta, under
        // the event names each item type uses.
        let mut added = ResponsePlan::responses_tool_call_item(tool_call);
        let (payload_key, delta_event, done_event) = if tool_call.custom {
            (
                "input",
                "response.custom_tool_call_input.delta",
                "response.custom_tool_call_input.done",
            )
        } else {
            (
                "arguments",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
            )
        };
        let item_id = added["id"].clone();
        let payload = ResponsePlan::tool_call_arguments_text(tool_call);
        added[payload_key] = json!("");
        events.push(sse_event(
            "response.output_item.added",
            &json!({
                "type": "response.output_item.added",
                "item": added,
                "output_index": next_output_index,
            }),
        ));
        events.push(sse_event(
            delta_event,
            &json!({
                "type": delta_event,
                "item_id": item_id,
                "delta": payload,
                "output_index": next_output_index,
            }),
        ));
        events.push(sse_event(
            done_event,
            &json!({
                "type": done_event,
                "item_id": item_id,
                payload_key: payload,
                "output_index": next_output_index,
            }),
        ));
        events.push(sse_event(
            "response.output_item.done",
            &json!({
                "type": "response.output_item.done",
                "item": ResponsePlan::responses_tool_call_item(tool_call),
                "output_index": next_output_index,
            }),
        ));
        next_output_index += 1;
    }

    if !transport.malformed_sse {
        let terminal = if plan.truncated {
            "response.incomplete"
        } else {
            "response.completed"
        };
        events.push(sse_event(
            terminal,
            &json!({
                "type": terminal,
                "response": plan.responses_json(),
            }),
        ));
    }

    stream_response(events, transport)
}

pub fn chat_sse_response(
    plan: &ResponsePlan,
    include_usage: bool,
    transport: TransportOptions,
) -> Response<Body> {
    let mut events = Vec::new();
    let content = plan.chat_content();
    events.push(chat_chunk(&json!({
        "id": format!("chatcmpl_{}", plan.id),
        "object": "chat.completion.chunk",
        "created": plan.created,
        "model": plan.model,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant"
            },
            "finish_reason": Value::Null,
        }]
    })));

    if !content.is_empty() {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {
                    "content": content
                },
                "finish_reason": Value::Null,
            }]
        })));
    }

    for reasoning in &plan.reasoning {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning": reasoning
                },
                "finish_reason": Value::Null,
            }]
        })));
    }

    if !plan.tool_calls.is_empty() {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": plan.tool_calls.iter().enumerate().map(|(index, tool_call)| json!({
                        "index": index,
                        "id": tool_call.id,
                        "type": "function",
                        "function": {
                            "name": tool_call.name,
                            "arguments": ResponsePlan::tool_call_arguments_text(tool_call),
                        }
                    })).collect::<Vec<_>>()
                },
                "finish_reason": Value::Null,
            }]
        })));
    }

    if !transport.malformed_sse {
        events.push(chat_chunk(&json!({
            "id": format!("chatcmpl_{}", plan.id),
            "object": "chat.completion.chunk",
            "created": plan.created,
            "model": plan.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": plan.chat_finish_reason(),
            }]
        })));
        if include_usage {
            events.push(chat_chunk(&json!({
                "id": format!("chatcmpl_{}", plan.id),
                "object": "chat.completion.chunk",
                "created": plan.created,
                "model": plan.model,
                "choices": [],
                "usage": plan.usage.chat_completions_json(),
            })));
        }
        events.push("data: [DONE]\n\n".to_owned());
    }

    stream_response(events, transport)
}

fn stream_response(events: Vec<String>, transport: TransportOptions) -> Response<Body> {
    let limit = transport.close_after_chunks.unwrap_or(events.len());
    let malformed_sse = transport.malformed_sse;
    let inter_event_delay_ms = transport.inter_event_delay_ms;

    let body = Body::from_stream(stream! {
        for (index, event) in events.into_iter().enumerate() {
            if index >= limit {
                break;
            }

            if inter_event_delay_ms > 0 {
                sleep(Duration::from_millis(inter_event_delay_ms)).await;
            }

            yield Ok::<_, std::convert::Infallible>(event.into_bytes());
        }

        if malformed_sse {
            yield Ok::<_, std::convert::Infallible>(b"event: malformed\ndata: {".to_vec());
        }
    });

    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn chat_chunk(data: &Value) -> String {
    format!("data: {data}\n\n")
}

/// Renders a recorded transcript exchange back exactly as captured.
///
/// A JSON transcript goes back as one body; an SSE transcript goes back as
/// its recorded events in order, one chunk per event, so a streaming client
/// sees the same event granularity the original provider sent.
pub fn transcript_response(outcome: TranscriptOutcome) -> Response<Body> {
    match outcome.body {
        TranscriptBody::Json(body) => {
            let content_type = outcome
                .content_type
                .unwrap_or_else(|| "application/json".to_owned());
            let rendered = body.to_string();
            build_response(outcome.status, &content_type, Body::from(rendered))
        }
        TranscriptBody::Events(events) => {
            let content_type = outcome
                .content_type
                .unwrap_or_else(|| "text/event-stream".to_owned());
            let body = Body::from_stream(stream! {
                for event in events {
                    yield Ok::<_, std::io::Error>(render_transcript_event(&event));
                }
            });
            build_response(outcome.status, &content_type, body)
        }
    }
}

fn build_response(status: StatusCode, content_type: &str, body: Body) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

/// One SSE event as its wire text. Multi-line data becomes one `data:` line
/// per line, which is how the SSE encoding carries embedded newlines.
fn render_transcript_event(event: &TranscriptEvent) -> String {
    let mut rendered = String::new();
    if let Some(name) = &event.event {
        rendered.push_str("event: ");
        rendered.push_str(name);
        rendered.push('\n');
    }
    for line in event.data.split('\n') {
        rendered.push_str("data: ");
        rendered.push_str(line);
        rendered.push('\n');
    }
    rendered.push('\n');
    rendered
}
