use serde_json::json;
use twin_openai::record::{
    derive_script, ExchangeShape, RecordedEndpoint, RecordedExchange, RecordedSseEvent,
};

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
