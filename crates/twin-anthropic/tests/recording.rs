use serde_json::{json, Value};
use twin_anthropic::engine::scenario::TranscriptEvent;
use twin_anthropic::record::{message_from_events, parse_sse_events, request_hash};

fn event(value: &Value) -> TranscriptEvent {
    TranscriptEvent {
        event: value["type"].as_str().map(str::to_owned),
        data: value.to_string(),
    }
}

fn message_start() -> TranscriptEvent {
    event(
        &json!({"type":"message_start","message":{"type":"message","content":[],"usage":{"input_tokens":1,"output_tokens":0},"stop_reason":null}}),
    )
}

fn text_stream() -> Vec<TranscriptEvent> {
    vec![
        message_start(),
        event(
            &json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        event(
            &json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello 🌊"}}),
        ),
        event(&json!({"type":"content_block_stop","index":0})),
        event(
            &json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
        ),
        event(&json!({"type":"message_stop"})),
    ]
}

#[test]
fn semantic_recording_checks_message_and_block_lifecycles() {
    let valid = text_stream();
    let message = message_from_events(&valid).expect("valid stream");
    assert_eq!(
        message["content"],
        json!([{"type":"text","text":"hello 🌊"}])
    );
    let mut bad_streams = Vec::new();
    for index in [0, 1, 3, 4, 5] {
        let mut events = valid.clone();
        events.remove(index);
        bad_streams.push(events);
    }
    for index in [0, 1, 3, 5] {
        let mut events = valid.clone();
        events.insert(index, events[index].clone());
        bad_streams.push(events);
    }
    for pair in [(0, 1), (2, 3), (3, 4)] {
        let mut events = valid.clone();
        events.swap(pair.0, pair.1);
        bad_streams.push(events);
    }
    for replacement in [
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"x"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
    ] {
        let mut events = valid.clone();
        events[2] = event(&replacement);
        bad_streams.push(events);
    }
    let mut mismatch = valid.clone();
    mismatch[2].event = Some("message_stop".to_owned());
    bad_streams.push(mismatch);
    for events in bad_streams {
        assert!(
            message_from_events(&events).is_err(),
            "accepted malformed stream: {events:?}"
        );
    }

    let mut updates = valid;
    updates.insert(2, event(&json!({"type":"ping"})));
    updates.insert(updates.len() - 1, event(&json!({"type":"message_delta","delta":{},"usage":{"output_tokens":3,"cache_read_input_tokens":7}})));
    let message = message_from_events(&updates).expect("cumulative updates");
    assert_eq!(
        message["usage"],
        json!({"input_tokens":1,"output_tokens":3,"cache_read_input_tokens":7})
    );
}

#[test]
fn tool_json_must_be_complete_and_an_object_when_the_block_closes() {
    for (raw, valid) in [
        ("{\"city\":\"Paris\"}", true),
        ("{\"city\":", false),
        ("[]", false),
        ("null", false),
    ] {
        let mut events = text_stream();
        events[1] = event(
            &json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_test","name":"weather","input":{}}}),
        );
        events[2] = event(
            &json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":raw}}),
        );
        let result = message_from_events(&events);
        assert_eq!(result.is_ok(), valid, "tool input: {raw}");
        if valid {
            assert_eq!(
                result.expect("valid tool input")["content"][0]["input"],
                json!({"city":"Paris"})
            );
        }
    }
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
        let stop = event(&json!({"type":"message_stop"}));
        for events in [
            vec![event(&invalid)],
            vec![
                event(&json!({"type":"message_start","message":invalid})),
                stop.clone(),
            ],
            vec![
                message_start(),
                event(&json!({"type":"content_block_start","index":0,"content_block":invalid})),
                event(
                    &json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}),
                ),
                stop.clone(),
            ],
            vec![
                message_start(),
                event(&json!({"type":"message_delta","delta":invalid})),
                stop.clone(),
            ],
            vec![
                message_start(),
                event(&json!({"type":"message_delta","delta":{},"usage":invalid})),
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
            &json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":42}}),
        ),
        event(
            &json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}),
        ),
        event(&json!({"type":"message_stop"})),
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
