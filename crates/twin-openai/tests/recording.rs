use serde_json::json;
use twin_openai::record::{
    derive_script, parse_sse_events, request_hash, ExchangeShape, RecordedEndpoint,
    RecordedExchange, RecordedSseEvent,
};

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
fn responses_only_records_token_limit_incompleteness_as_length() {
    for (status, reason, expected) in [
        ("incomplete", Some("max_output_tokens"), Some("length")),
        ("incomplete", Some("content_filter"), None),
        ("incomplete", None, None),
        ("completed", None, None),
    ] {
        let script = derive_script(
            ExchangeShape {
                endpoint: RecordedEndpoint::Responses,
                stream: false,
                structured: false,
            },
            &RecordedExchange::Json(json!({
                "status": status,
                "incomplete_details": { "reason": reason },
                "output": []
            })),
        )
        .expect("response should derive");
        assert_eq!(
            script
                .get("finish_reason")
                .and_then(serde_json::Value::as_str),
            expected
        );
    }
}

#[test]
fn chat_records_terminal_reason_without_a_delta_and_keeps_trailing_usage() {
    let events = [
        json!({ "choices": [{ "delta": { "content": "cut off" } }] }).to_string(),
        json!({ "choices": [{ "finish_reason": "length" }] }).to_string(),
        json!({
            "choices": [],
            "usage": { "prompt_tokens": 9, "completion_tokens": 2 }
        })
        .to_string(),
        "[DONE]".to_owned(),
    ]
    .into_iter()
    .map(|data| RecordedSseEvent { event: None, data })
    .collect();
    let script = derive_script(
        ExchangeShape {
            endpoint: RecordedEndpoint::ChatCompletions,
            stream: true,
            structured: false,
        },
        &RecordedExchange::Stream(events),
    )
    .expect("chat stream should derive");
    assert_eq!(script["finish_reason"], "length");
    assert_eq!(script["response_text"], "cut off");
    assert_eq!(
        script["usage"],
        json!({ "input_tokens": 9, "output_tokens": 2 })
    );
}
