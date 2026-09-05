use serde_json::{json, Value};
use twin_anthropic::engine::scenario::{validate_scenario_ids, ScenarioEnvelope, TranscriptEvent};
use twin_anthropic::record::{derive_script, message_from_events};

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
fn derived_scripts_must_pass_replay_startup_validation() {
    let good = json!({"type":"message","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2,"vendor_usage":{"units":3}}});
    for (field, invalid) in [
        ("usage", json!({"input_tokens":"bad","output_tokens":1})),
        ("usage", json!({"input_tokens":-1})),
        ("usage", json!([])),
        ("stop_sequence", json!(42)),
        ("stop_reason", json!("vendor_future_reason")),
    ] {
        let mut body = good.clone();
        body[field] = invalid;
        assert!(
            derive_script(&body).is_err(),
            "accepted invalid response: {body}"
        );
    }
    let script = derive_script(&good).expect("valid derived script");
    assert_eq!(script["usage"], good["usage"]);
    let envelope: ScenarioEnvelope = serde_json::from_value(
        json!({"scenarios":[{"matcher":{"endpoint":"messages"},"script":script}]}),
    )
    .expect("loadable fixture");
    validate_scenario_ids(&envelope.scenarios).expect("valid fixture");
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
