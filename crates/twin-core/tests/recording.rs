use twin_core::record::request_hash;
use twin_core::sse::{parse_sse_events, render_event};

#[test]
fn request_hash_ignores_whitespace_and_recursive_object_key_order() {
    let first = br#"{"model":"gpt-test","messages":[{"role":"user","content":"hello"}],"metadata":{"b":2,"a":1}}"#;
    let reordered = br#"{
        "metadata": {"a": 1, "b": 2},
        "messages": [{"content": "hello", "role": "user"}],
        "model": "gpt-test"
    }"#;

    let hash = request_hash(first).expect("valid request JSON");
    assert_eq!(Some(hash), request_hash(reordered));
}

#[test]
fn request_hash_preserves_array_order_and_values() {
    let hash = request_hash(br#"{"input":["first","second"]}"#).expect("valid JSON");
    assert_ne!(
        Some(hash.clone()),
        request_hash(br#"{"input":["second","first"]}"#)
    );
    assert_ne!(Some(hash), request_hash(br#"{"input":["first","third"]}"#));
    assert_eq!(request_hash(b"not JSON"), None);
}

#[test]
fn sse_accepts_supported_line_endings_and_optional_spaces() {
    for newline in ["\n", "\r\n", "\r"] {
        for space in ["", " "] {
            let body = [
                ": keepalive".to_owned(),
                format!("event:{space}response.output_text.delta"),
                "id: 123".to_owned(),
                format!("data:{space}first"),
                format!("data:{space}second"),
                String::new(),
                format!("data:{space}[DONE]"),
                String::new(),
                String::new(),
            ]
            .join(newline);
            let events = parse_sse_events(body.as_bytes()).expect("valid SSE");
            assert_eq!(events.len(), 2, "body: {body:?}");
            assert_eq!(
                events[0].event.as_deref(),
                Some("response.output_text.delta")
            );
            assert_eq!(events[0].data, "first\nsecond");
            assert_eq!(events[1].event, None);
            assert_eq!(events[1].data, "[DONE]");
        }
    }
}

#[test]
fn sse_preserves_empty_data_and_leading_spaces() {
    let body =
        "\u{feff}event: unused\n\n: comment\r\rdata\r\n\r\nevent: last\ndata:  spaced\ndata:";
    let events = parse_sse_events(body.as_bytes()).expect("valid SSE");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, None);
    assert_eq!(events[0].data, "");
    assert_eq!(events[1].event.as_deref(), Some("last"));
    assert_eq!(events[1].data, " spaced\n");
    assert!(parse_sse_events(&[0xff]).is_err());
}

#[test]
fn rendered_events_preserve_names_multiline_data_and_empty_data() {
    let original =
        parse_sse_events(b"event: message_start\ndata: first\ndata: second\n\ndata:\n\n")
            .expect("captured events");
    let wire = original.iter().map(render_event).collect::<String>();
    let replay = parse_sse_events(wire.as_bytes()).expect("replayed events");
    assert_eq!(replay.len(), original.len());
    for (actual, expected) in replay.iter().zip(&original) {
        assert_eq!(actual.event, expected.event);
        assert_eq!(actual.data, expected.data);
    }
}
