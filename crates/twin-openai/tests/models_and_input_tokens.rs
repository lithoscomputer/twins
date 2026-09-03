//! Black-box HTTP coverage for model discovery and Responses input counts.

mod common;

use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};

async fn error_body(response: reqwest::Response) -> Value {
    assert_eq!(response.status(), 400);
    response.json().await.expect("error body should be JSON")
}

async fn input_token_count(server: &common::TestServer, body: &Value) -> u64 {
    let response = server
        .auth_client
        .post(format!("{}/v1/responses/input_tokens", server.base_url))
        .json(body)
        .send()
        .await
        .expect("input-token request should complete");
    assert_eq!(response.status(), 200);
    let response: Value = response.json().await.expect("count body should be JSON");
    assert_eq!(response["object"], "response.input_tokens");
    assert_eq!(response.as_object().map(serde_json::Map::len), Some(2));
    response["input_tokens"]
        .as_u64()
        .expect("input_tokens should be an unsigned integer")
}

#[tokio::test]
async fn platform_routes_require_bearer_auth() {
    let server = common::spawn_server().await.expect("server should start");

    let models = server
        .client
        .get(format!("{}/v1/models", server.base_url))
        .send()
        .await
        .expect("models request should complete");
    assert_eq!(models.status(), 401);
    let models: Value = models.json().await.expect("auth error should be JSON");
    assert_eq!(models["error"]["code"], "missing_bearer_token");

    let count = server
        .client
        .post(format!("{}/v1/responses/input_tokens", server.base_url))
        .json(&json!({ "model": "gpt-test", "input": "hello" }))
        .send()
        .await
        .expect("input-token request should complete");
    assert_eq!(count.status(), 401);
    let count: Value = count.json().await.expect("auth error should be JSON");
    assert_eq!(count["error"]["code"], "missing_bearer_token");
}

#[tokio::test]
async fn models_returns_a_stable_openai_list() {
    let server = common::spawn_server().await.expect("server should start");

    let response = server
        .auth_client
        .get(format!("{}/v1/models", server.base_url))
        .send()
        .await
        .expect("models request should complete");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let body: Value = response.json().await.expect("models body should be JSON");
    assert_eq!(
        body,
        json!({
            "object": "list",
            "data": [{
                "id": "gpt-test",
                "object": "model",
                "created": 0,
                "owned_by": "twin-openai",
                "shutdown_date": null
            }]
        })
    );

    let other_namespace = server
        .fork_namespace()
        .expect("second namespace should build");
    let second: Value = other_namespace
        .auth_client
        .get(format!("{}/v1/models", server.base_url))
        .send()
        .await
        .expect("second models request should complete")
        .json()
        .await
        .expect("second models body should be JSON");
    assert_eq!(second, body);
}

#[tokio::test]
async fn input_token_count_is_stable_and_covers_the_complete_request() {
    let server = common::spawn_server().await.expect("server should start");
    let body = json!({
        "model": "gpt-test",
        "instructions": "Answer briefly.",
        "input": [{
            "role": "user",
            "content": [{ "type": "input_text", "text": "What is the weather?" }]
        }],
        "parallel_tool_calls": true,
        "reasoning": { "effort": "low" },
        "text": { "format": { "type": "text" } },
        "tool_choice": "auto",
        "tools": [{
            "type": "function",
            "name": "weather",
            "description": "Look up the weather",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
                "additionalProperties": false
            },
            "strict": true
        }],
        "truncation": "disabled"
    });

    let first = input_token_count(&server, &body).await;
    let repeated = input_token_count(&server, &body).await;
    assert_eq!(first, repeated);
    assert!(first > 0);

    let reordered = json!({
        "truncation": "disabled",
        "tools": body["tools"],
        "tool_choice": "auto",
        "text": body["text"],
        "reasoning": body["reasoning"],
        "parallel_tool_calls": true,
        "input": body["input"],
        "instructions": "Answer briefly.",
        "model": "gpt-test"
    });
    assert_eq!(input_token_count(&server, &reordered).await, first);

    let mut longer = body;
    longer["instructions"] = Value::String(
        "Answer briefly, but first consider every supplied instruction and tool definition."
            .to_owned(),
    );
    assert!(input_token_count(&server, &longer).await > first);
}

#[tokio::test]
async fn input_token_count_rejects_invalid_requests_with_openai_errors() {
    let server = common::spawn_server().await.expect("server should start");
    let url = format!("{}/v1/responses/input_tokens", server.base_url);

    let missing_model = server
        .auth_client
        .post(&url)
        .json(&json!({ "input": "hello" }))
        .send()
        .await
        .expect("missing-model request should complete");
    let missing_model = error_body(missing_model).await;
    assert_eq!(missing_model["error"]["type"], "invalid_request_error");
    assert_eq!(missing_model["error"]["param"], "body");

    let empty_model = server
        .auth_client
        .post(&url)
        .json(&json!({ "model": "  ", "input": "hello" }))
        .send()
        .await
        .expect("empty-model request should complete");
    let empty_model = error_body(empty_model).await;
    assert_eq!(empty_model["error"]["param"], "model");

    let malformed = server
        .auth_client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .body("{")
        .send()
        .await
        .expect("malformed request should complete");
    let malformed = error_body(malformed).await;
    assert_eq!(malformed["error"]["param"], "body");

    let invalid_input = server
        .auth_client
        .post(&url)
        .json(&json!({
            "model": "gpt-test",
            "input": [{ "role": "user", "content": [] }]
        }))
        .send()
        .await
        .expect("invalid-input request should complete");
    let invalid_input = error_body(invalid_input).await;
    assert_eq!(invalid_input["error"]["param"], "input");
}

#[tokio::test]
async fn utility_calls_do_not_consume_generation_state() {
    let server = common::spawn_server().await.expect("server should start");
    server
        .enqueue_scenarios(json!({
            "scenarios": [{
                "scenario_id": "still-available",
                "matcher": {
                    "endpoint": "responses",
                    "model": "gpt-test",
                    "stream": false
                },
                "script": { "kind": "success", "response_text": "scripted" }
            }]
        }))
        .await;

    let models = server
        .auth_client
        .get(format!("{}/v1/models", server.base_url))
        .send()
        .await
        .expect("models request should complete");
    assert_eq!(models.status(), 200);
    assert!(
        input_token_count(&server, &json!({ "model": "gpt-test", "input": "hello" })).await > 0
    );

    let generation = server
        .post_responses(json!({ "model": "gpt-test", "input": "hello" }))
        .await;
    assert_eq!(generation.status(), 200);
    let generation: Value = generation.json().await.expect("response should be JSON");
    assert_eq!(generation["id"], "resp_000001");
    assert_eq!(generation["output"][0]["content"][0]["text"], "scripted");

    let logs = server.request_logs().await;
    let requests = logs["requests"]
        .as_array()
        .expect("requests should be an array");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["scenario_id"], "still-available");
}

#[tokio::test]
async fn utility_routes_reject_the_wrong_http_methods() {
    let server = common::spawn_server().await.expect("server should start");

    let post_models = server
        .auth_client
        .post(format!("{}/v1/models", server.base_url))
        .send()
        .await
        .expect("POST models request should complete");
    assert_eq!(post_models.status(), 405);

    let get_count = server
        .auth_client
        .get(format!("{}/v1/responses/input_tokens", server.base_url))
        .send()
        .await
        .expect("GET input-token request should complete");
    assert_eq!(get_count.status(), 405);
}
