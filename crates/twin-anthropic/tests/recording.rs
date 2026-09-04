use twin_anthropic::record::{parse_sse_events, request_hash};

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
