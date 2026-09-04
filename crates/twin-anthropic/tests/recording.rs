use serde_json::{json, Value};
use twin_anthropic::engine::scenario::TranscriptEvent;
use twin_anthropic::record::{message_from_events, parse_sse_events, request_hash};

fn event(value: Value) -> TranscriptEvent {
    TranscriptEvent {
        event: value["type"].as_str().map(str::to_owned),
        data: value.to_string(),
    }
}

fn message_start() -> TranscriptEvent {
    event(
        json!({"type":"message_start","message":{"type":"message","content":[],"usage":{"input_tokens":1,"output_tokens":0},"stop_reason":null}}),
    )
}

#[test]
fn malformed_event_shapes_return_errors_without_panicking() {
    for invalid in [
        Value::Null,
        json!(false),
        json!(42),
        json!("bad"),
        json!([]),
    ] {
        let stop = event(json!({"type":"message_stop"}));
        for events in [
            vec![event(invalid.clone())],
            vec![
                event(json!({"type":"message_start","message":invalid})),
                stop.clone(),
            ],
            vec![
                message_start(),
                event(json!({"type":"content_block_start","index":0,"content_block":invalid})),
                event(
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}),
                ),
                stop.clone(),
            ],
            vec![
                message_start(),
                event(json!({"type":"message_delta","delta":invalid})),
                stop.clone(),
            ],
            vec![
                message_start(),
                event(json!({"type":"message_delta","delta":{},"usage":invalid})),
                stop,
            ],
        ] {
            assert!(
                message_from_events(&events).is_err(),
                "invalid value {invalid}"
            );
        }
    }
    let events = vec![
        message_start(),
        event(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":42}}),
        ),
        event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}),
        ),
        event(json!({"type":"message_stop"})),
    ];
    assert!(message_from_events(&events).is_err());
}

#[test]
fn transcript_hash_ignores_object_order_but_preserves_array_order() {
    let a=br#"{"model":"claude-test","messages":[{"role":"user","content":"hello"}],"metadata":{"b":2,"a":1}}"#;
    let b=br#"{ "metadata": {"a":1,"b":2}, "messages":[{"content":"hello","role":"user"}],"model":"claude-test" }"#;
    assert_eq!(
        request_hash(a).expect("JSON"),
        request_hash(b).expect("JSON")
    );
    assert_ne!(
        request_hash(br#"{"items":[1,2]}"#),
        request_hash(br#"{"items":[2,1]}"#)
    );
    assert_eq!(request_hash(b"bad JSON"), None);
}

#[test]
fn recording_accepts_sse_line_endings_and_optional_spaces() {
    for newline in ["\n", "\r\n", "\r"] {
        for space in ["", " "] {
            let body = [
                format!("event:{space}message_start"),
                format!("data:{space}first"),
                format!("data:{space}second"),
                String::new(),
                ": ping".to_owned(),
                "data:".to_owned(),
                String::new(),
                String::new(),
            ]
            .join(newline);
            let events = parse_sse_events(body.as_bytes()).expect("SSE");
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].event.as_deref(), Some("message_start"));
            assert_eq!(events[0].data, "first\nsecond");
            assert_eq!(events[1].event, None);
            assert_eq!(events[1].data, "");
        }
    }
    assert!(parse_sse_events(&[255]).is_err());
}
