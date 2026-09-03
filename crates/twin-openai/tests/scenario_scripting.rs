//! Scenario features a black-box client suite leans on: repeated and sticky
//! answers, matching on instructions, scripted truncation, custom tool calls,
//! and empty tool outputs.

mod common;

use serde_json::json;

fn text_of(body: &serde_json::Value) -> &str {
    body["output"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_repeat_scenario_answers_that_many_times_then_is_spent() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "error", "status": 500, "message": "still down", "error_type": "server_error", "code": "server_error" },
                    "repeat": 2
                },
                {
                    "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                    "script": { "kind": "success", "response_text": "back up" }
                }
            ]
        }))
        .await;

    let request = || json!({ "model": "gpt-test", "input": "hello", "stream": false });
    assert_eq!(server.post_responses(request()).await.status(), 500);
    assert_eq!(server.post_responses(request()).await.status(), 500);
    let third = server.post_responses(request()).await;
    assert_eq!(third.status(), 200);
    let body = third.json::<serde_json::Value>().await.expect("json");
    assert_eq!(text_of(&body), "back up");
}

#[tokio::test]
async fn a_sticky_scenario_answers_until_the_namespace_is_reset() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [{
                "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                "script": { "kind": "success", "response_text": "always" },
                "sticky": true
            }]
        }))
        .await;

    let request = || json!({ "model": "gpt-test", "input": "hello", "stream": false });
    for _ in 0..3 {
        let body = server
            .post_responses(request())
            .await
            .json::<serde_json::Value>()
            .await
            .expect("json");
        assert_eq!(text_of(&body), "always");
    }

    server.reset().await;
    let body = server
        .post_responses(request())
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(text_of(&body), "deterministic: hello");
}

#[tokio::test]
async fn instructions_contains_matches_the_field_and_system_messages() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "instructions_contains": "answer in French" },
                    "script": { "kind": "success", "response_text": "Bonjour" }
                },
                {
                    "matcher": { "endpoint": "chat.completions", "instructions_contains": "answer in German" },
                    "script": { "kind": "success", "response_text": "Hallo" }
                }
            ]
        }))
        .await;

    // The system prompt as an input item, the way a client without a
    // dedicated instructions field sends it.
    let via_item = server
        .post_responses(json!({
            "model": "gpt-test",
            "stream": false,
            "input": [
                { "role": "system", "content": "Always answer in French." },
                { "role": "user", "content": "hello" }
            ]
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(text_of(&via_item), "Bonjour");

    let via_chat = server
        .post_chat(json!({
            "model": "gpt-test",
            "stream": false,
            "messages": [
                { "role": "system", "content": "Always answer in German." },
                { "role": "user", "content": "hello" }
            ]
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(via_chat["choices"][0]["message"]["content"], "Hallo");

    // A request whose instructions do not match falls through to the default.
    let unmatched = server
        .post_responses(json!({
            "model": "gpt-test",
            "stream": false,
            "instructions": "Be terse.",
            "input": "hello"
        }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(text_of(&unmatched), "deterministic: hello");

    let logs = server.request_logs().await;
    assert_eq!(
        logs["requests"][0]["instructions_text"],
        "Always answer in French."
    );
}

#[tokio::test]
async fn a_length_finish_reason_renders_an_incomplete_response() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "stream": false },
                    "script": { "kind": "success", "response_text": "cut off mid", "finish_reason": "length" }
                },
                {
                    "matcher": { "endpoint": "responses", "stream": true },
                    "script": { "kind": "success", "response_text": "cut off mid", "finish_reason": "length" }
                },
                {
                    "matcher": { "endpoint": "chat.completions" },
                    "script": { "kind": "success", "response_text": "cut off mid", "finish_reason": "length" }
                }
            ]
        }))
        .await;

    let blocking = server
        .post_responses(json!({ "model": "gpt-test", "input": "go on", "stream": false }))
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(blocking["status"], "incomplete");
    assert_eq!(
        blocking["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(text_of(&blocking), "cut off mid");

    let (status, chunks) = server
        .post_responses_stream(json!({ "model": "gpt-test", "input": "go on", "stream": true }))
        .await;
    assert_eq!(status, 200);
    let joined = chunks.join("");
    let transcript = common::parse_sse_transcript(joined.as_bytes()).expect("valid sse");
    let last = transcript
        .events
        .last()
        .and_then(|event| event.event.as_deref());
    assert_eq!(last, Some("response.incomplete"));
    assert!(!joined.contains("response.completed"));

    let chat = server
        .post_chat(
            json!({ "model": "gpt-test", "messages": [{ "role": "user", "content": "go on" }] }),
        )
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(chat["choices"][0]["finish_reason"], "length");
}

#[tokio::test]
async fn a_custom_tool_call_streams_as_custom_tool_call_input() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": { "endpoint": "responses", "stream": false },
                    "script": {
                        "kind": "success",
                        "tool_calls": [{
                            "id": "call_patch",
                            "name": "apply_patch",
                            "kind": "custom",
                            "arguments": "*** Begin Patch\n*** End Patch"
                        }]
                    }
                },
                {
                    "matcher": { "endpoint": "responses", "stream": true },
                    "script": {
                        "kind": "success",
                        "tool_calls": [{
                            "id": "call_patch",
                            "name": "apply_patch",
                            "kind": "custom",
                            "arguments": "*** Begin Patch\n*** End Patch"
                        }]
                    }
                }
            ]
        }))
        .await;

    let tools = json!([{ "type": "custom", "name": "apply_patch" }]);
    let blocking = server
        .post_responses(
            json!({ "model": "gpt-test", "input": "patch it", "stream": false, "tools": tools }),
        )
        .await
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(blocking["output"][0]["type"], "custom_tool_call");
    assert_eq!(blocking["output"][0]["id"], "ctc_call_patch");
    assert_eq!(blocking["output"][0]["call_id"], "call_patch");
    assert_eq!(blocking["output"][0]["name"], "apply_patch");
    assert_eq!(
        blocking["output"][0]["input"],
        "*** Begin Patch\n*** End Patch"
    );
    assert!(blocking["output"][0].get("arguments").is_none());

    let (status, chunks) = server
        .post_responses_stream(
            json!({ "model": "gpt-test", "input": "patch it", "stream": true, "tools": tools }),
        )
        .await;
    assert_eq!(status, 200);
    let joined = chunks.join("");
    let transcript = common::parse_sse_transcript(joined.as_bytes()).expect("valid sse");
    let events = transcript
        .events
        .iter()
        .filter_map(|event| event.event.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.output_item.done",
            "response.output_item.added",
            "response.custom_tool_call_input.delta",
            "response.custom_tool_call_input.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    assert!(joined.contains("\"type\":\"custom_tool_call\""));
    assert!(!joined.contains("function_call_arguments"));
}

#[tokio::test]
async fn an_empty_function_call_output_is_accepted() {
    let server = common::spawn_server().await.expect("server should start");

    // The live API accepts an empty output for a tool that produced nothing;
    // a missing output field is still rejected.
    let accepted = server
        .post_responses(json!({
            "model": "gpt-test",
            "stream": false,
            "input": [
                { "role": "user", "content": "list files" },
                { "type": "function_call", "call_id": "call_ls", "name": "glob", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "call_ls", "output": "" }
            ]
        }))
        .await;
    assert_eq!(accepted.status(), 200);

    let rejected = server
        .post_responses(json!({
            "model": "gpt-test",
            "stream": false,
            "input": [
                { "role": "user", "content": "list files" },
                { "type": "function_call", "call_id": "call_ls", "name": "glob", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "call_ls" }
            ]
        }))
        .await;
    assert_eq!(rejected.status(), 400);
    let body = rejected.json::<serde_json::Value>().await.expect("json");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("require output"));
}
