mod common;
use common::{config, request, scenario, spawn, text};
use serde_json::{json, Value};
use twin_anthropic::record::{message_from_events, parse_sse_events};

#[tokio::test]
async fn health_models_and_auth() {
    let server = spawn(config()).await;
    assert_eq!(
        server
            .client
            .get(format!("{}/healthz", server.url))
            .send()
            .await
            .expect("health")
            .status(),
        200
    );
    for path in ["/v1/messages", "/v1/messages/count_tokens"] {
        let response = server
            .client
            .post(format!("{}{path}", server.url))
            .json(&request(false))
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), 401);
        let body: Value = response.json().await.expect("error");
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "authentication_error");
    }
    let models: Value = server
        .get("/v1/models", "a")
        .await
        .json()
        .await
        .expect("models");
    assert_eq!(models["data"][0]["id"], "claude-test");
    assert_eq!(models["has_more"], false);
    assert_eq!(server.get("/v1/messages", "a").await.status(), 405);
    let no_version = server
        .client
        .post(format!("{}/v1/messages", server.url))
        .header("x-api-key", "a")
        .json(&request(false))
        .send()
        .await
        .expect("post");
    assert_eq!(no_version.status(), 400);
}

#[tokio::test]
async fn deterministic_ids_and_namespace_isolation() {
    let server = spawn(config()).await;
    assert_eq!(
        server.message("a", &request(false)).await["id"],
        "msg_000001"
    );
    assert_eq!(
        server.message("a", &request(false)).await["id"],
        "msg_000002"
    );
    let b = server.message("b", &request(false)).await;
    assert_eq!(b["id"], "msg_000001");
    assert_eq!(text(&b), "deterministic: hello");
    assert_eq!(b["type"], "message");
    assert_eq!(b["stop_reason"], "end_turn");
    assert_eq!(
        server.logs("a").await["requests"]
            .as_array()
            .expect("logs")
            .len(),
        2
    );
}

#[tokio::test]
async fn json_and_stream_share_native_content_and_usage() {
    let server = spawn(config()).await;
    let content = json!([
        {"type":"thinking","thinking":"considering","signature":"signed-thought"},
        {"type":"redacted_thinking","data":"encrypted"},
        {"type":"text","text":"hello 🌊"},
        {"type":"tool_use","id":"toolu_1","name":"weather","input":{"city":"Paris"}}
    ]);
    let script = json!({"kind":"success","content":content,"usage":{"input_tokens":9,"output_tokens":4,"cache_creation_input_tokens":7,"cache_read_input_tokens":3,"cache_creation":{"ephemeral_5m_input_tokens":7,"ephemeral_1h_input_tokens":0}}});
    server
        .enqueue("json", json!([scenario(script.clone())]))
        .await;
    server.enqueue("stream", json!([scenario(script)])).await;
    let expected = server.message("json", &request(false)).await;
    let response = server.post("/v1/messages", "stream", &request(true)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let bytes = response.bytes().await.expect("stream");
    let events = parse_sse_events(&bytes).expect("events");
    assert_eq!(
        events.first().expect("start").event.as_deref(),
        Some("message_start")
    );
    assert_eq!(
        events.last().expect("stop").event.as_deref(),
        Some("message_stop")
    );
    assert!(!bytes.windows(6).any(|s| s == b"[DONE]"));
    assert_eq!(message_from_events(&events).expect("message"), expected);
}

#[tokio::test]
async fn thinking_and_tools_round_trip_as_conversation_history() {
    let server = spawn(config()).await;
    server.enqueue("a",json!([scenario(json!({"kind":"success","content":[{"type":"thinking","thinking":"hmm","signature":"sig"},{"type":"tool_use","id":"toolu_a","name":"lookup","input":{}}]}))])).await;
    let first = server.message("a", &request(false)).await;
    let followup = json!({"model":"claude-test","max_tokens":128,"messages":[{"role":"user","content":"hello"},{"role":"assistant","content":first["content"]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_a","content":""}]}]});
    assert_eq!(
        server.post("/v1/messages", "a", &followup).await.status(),
        200
    );
}

#[tokio::test]
async fn supported_stop_reasons_survive_json_and_streams() {
    let server = spawn(config()).await;
    for reason in [
        "end_turn",
        "max_tokens",
        "stop_sequence",
        "tool_use",
        "pause_turn",
        "refusal",
        "model_context_window_exceeded",
    ] {
        for stream in [false, true] {
            server.enqueue("a",json!([scenario(json!({"kind":"success","response_text":"partial","stop_reason":reason,"stop_sequence":"END","stop_details":{"explanation":"scripted"}}))])).await;
            let response = server.post("/v1/messages", "a", &request(stream)).await;
            assert_eq!(response.status(), 200);
            let bytes = response.bytes().await.expect("body");
            let body: Value = if stream {
                message_from_events(&parse_sse_events(&bytes).expect("events")).expect("message")
            } else {
                serde_json::from_slice(&bytes).expect("JSON")
            };
            assert_eq!(body["stop_reason"], reason);
            assert_eq!(body["stop_details"]["explanation"], "scripted");
        }
    }
}

#[tokio::test]
async fn structured_output_and_forced_tools() {
    let server = spawn(config()).await;
    let mut req = request(false);
    req["output_config"] = json!({"format":{"type":"json_schema","schema":{"type":"object","properties":{"answer":{"type":"string"},"ok":{"type":"boolean"},"kind":{"enum":["result"]}}}}});
    let body = server.message("a", &req).await;
    let parsed: Value = serde_json::from_str(text(&body)).expect("structured JSON");
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["kind"], "result");
    req["output_config"] = Value::Null;
    req["tools"] = json!([{"name":"weather","input_schema":{"type":"object"}}]);
    req["tool_choice"] = json!({"type":"tool","name":"weather"});
    assert_eq!(server.post("/v1/messages", "a", &req).await.status(), 400);
    server.enqueue("a",json!([scenario(json!({"kind":"success","tool_calls":[{"name":"weather","arguments":{"city":"Paris"}}]}))])).await;
    let body = server.message("a", &req).await;
    assert_eq!(body["stop_reason"], "tool_use");
    assert_eq!(body["content"].as_array().expect("blocks").len(), 1);
}

#[tokio::test]
async fn validation_rejects_malformed_requests() {
    let server = spawn(config()).await;
    for (field, value) in [
        ("model", json!("")),
        ("max_tokens", json!(0)),
        ("messages", json!([])),
        ("messages", json!([{"role":"tool","content":"bad"}])),
        ("messages", json!([{"role":"user","content":null}])),
        ("tool_choice", json!({"type":"required"})),
        ("thinking", json!({"type":"enabled","budget_tokens":2})),
        (
            "output_config",
            json!({"format":{"type":"json_schema","schema":{"type":"array"}}}),
        ),
    ] {
        let mut req = request(false);
        req[field] = value;
        let response = server.post("/v1/messages", "a", &req).await;
        assert_eq!(response.status(), 400, "field: {field}");
        assert_eq!(
            response.json::<Value>().await.expect("error")["error"]["type"],
            "invalid_request_error"
        );
    }
    let response = server
        .client
        .post(format!("{}/v1/messages", server.url))
        .header("x-api-key", "a")
        .header("anthropic-version", "2023-06-01")
        .body("{bad")
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn count_tokens_has_a_separate_request_contract_and_no_generation_side_effects() {
    let server = spawn(config()).await;
    let req = json!({"model":"claude-test","messages":[{"role":"user","content":"🌊"}]});
    let count: Value = server
        .post("/v1/messages/count_tokens", "a", &req)
        .await
        .json()
        .await
        .expect("count");
    assert_eq!(count["input_tokens"], req.to_string().len().div_ceil(4));
    assert_eq!(
        server
            .post("/v1/messages/count_tokens", "a", &request(false))
            .await
            .status(),
        400
    );
    server.enqueue("a",json!([{"matcher":{"endpoint":"messages.count_tokens"},"script":{"kind":"success","input_tokens":321}}])).await;
    assert_eq!(
        server
            .post("/v1/messages/count_tokens", "a", &req)
            .await
            .json::<Value>()
            .await
            .expect("count")["input_tokens"],
        321
    );
    assert_eq!(
        server.message("a", &request(false)).await["id"],
        "msg_000001"
    );
}

#[tokio::test]
async fn lithos_llm_wire_request_contracts() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/lithos-llm-requests.json")).expect("corpus");
    let server = spawn(config()).await;
    for case in cases {
        let req = &case["body"];
        let choice = req["tool_choice"]["type"].as_str();
        if matches!(choice, Some("tool" | "any")) {
            let name = req["tool_choice"]
                .get("name")
                .unwrap_or(&req["tools"][0]["name"]);
            server
                .enqueue(
                    "corpus",
                    json!([scenario(
                        json!({"kind":"success","tool_calls":[{"name":name,"arguments":{}}]})
                    )]),
                )
                .await;
        }
        let response = server
            .post(case["path"].as_str().expect("path"), "corpus", req)
            .await;
        let status = response.status();
        let body = response.text().await.expect("body");
        assert_eq!(
            u64::from(status.as_u16()),
            case["expected_status"].as_u64().unwrap_or(200),
            "case {}: {body}",
            case["case_id"]
        );
    }
}
