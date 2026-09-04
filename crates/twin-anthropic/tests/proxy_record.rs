mod common;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, Response},
    routing::{get, post},
    Router,
};
use common::{config, request, scenario, spawn, spawn_router, Server, TempFile};
use serde_json::{json, Value};
use twin_anthropic::config::{Config, Mode, RecordFormat};
use twin_anthropic::record::{message_from_events, parse_sse_events};

fn proxy_config(upstream: &Server, file: &TempFile, format: RecordFormat, append: bool) -> Config {
    Config {
        mode: Mode::ProxyRecord,
        upstream_url: upstream.url.clone(),
        upstream_api_key: Some("upstream-secret".to_owned()),
        recording_path: Some(file.0.clone()),
        record_format: format,
        recording_append: append,
        ..config()
    }
}
fn fixture(file: &TempFile) -> Value {
    serde_json::from_slice(&std::fs::read(&file.0).expect("recording")).expect("fixture")
}

#[tokio::test]
async fn semantic_proxy_replays_text_thinking_tools_usage_and_stop_reasons() {
    let upstream = spawn(config()).await;
    let file = TempFile::new();
    let content = json!([{ "type":"thinking","thinking":"hmm","signature":"sig" },{"type":"text","text":"partial"},{"type":"tool_use","id":"toolu_live","name":"weather","input":{"city":"Paris"}}]);
    let mut script = scenario(
        json!({"kind":"success","content":content,"stop_reason":"max_tokens","usage":{"input_tokens":9,"output_tokens":4,"cache_creation_input_tokens":7,"cache_read_input_tokens":3}}),
    );
    script["sticky"] = json!(true);
    upstream.enqueue("upstream-secret", json!([script])).await;
    let proxy = spawn(proxy_config(
        &upstream,
        &file,
        RecordFormat::Semantic,
        false,
    ))
    .await;
    for stream in [false, true] {
        let response = proxy.post("/v1/messages", "suite", &request(stream)).await;
        assert_eq!(response.status(), 200);
        response.bytes().await.expect("body");
    }
    assert_eq!(
        fixture(&file)["scenarios"]
            .as_array()
            .expect("scenarios")
            .len(),
        2
    );
    assert!(!std::fs::read_to_string(&file.0)
        .expect("file")
        .contains("upstream-secret"));
    let replay = spawn(Config {
        scenarios_path: Some(file.0.clone()),
        ..config()
    })
    .await;
    for stream in [false, true] {
        let response = replay.post("/v1/messages", "suite", &request(stream)).await;
        assert_eq!(response.status(), 200);
        let bytes = response.bytes().await.expect("body");
        let body: Value = if stream {
            message_from_events(&parse_sse_events(&bytes).expect("events")).expect("message")
        } else {
            serde_json::from_slice(&bytes).expect("JSON")
        };
        assert_eq!(body["content"], content);
        assert_eq!(body["stop_reason"], "max_tokens");
        assert_eq!(body["usage"]["cache_creation_input_tokens"], 7);
    }
    assert_eq!(
        replay
            .post("/v1/messages", "other", &request(false))
            .await
            .status(),
        400
    );
    for path in ["/__debug", "/__admin/requests"] {
        assert_eq!(proxy.get(path, "suite").await.status(), 404);
    }
}

async fn gateway(headers: HeaderMap, body: Bytes) -> Response<Body> {
    assert_eq!(headers["x-api-key"], "upstream-secret");
    assert!(headers.get("authorization").is_none());
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    let req: Value = serde_json::from_slice(&body).expect("request");
    if req["stream"] == true {
        Response::builder().header("content-type","text/event-stream").header("request-id","req_gateway").body(Body::from("event:message_start\r\ndata:{\"type\":\"message_start\",\"message\":{\"id\":\"msg_gateway\",\"content\":[],\"usage\":{\"input_tokens\":9}}}\r\n\r\nevent:message_delta\r\ndata:{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"usage\":{\"output_tokens\":1},\"vendor_extension\":true}\r\n\r\nevent:message_stop\r\ndata:{\"type\":\"message_stop\"}\r\n\r\n")).expect("response")
    } else {
        assert_eq!(headers["anthropic-beta"], "test-beta");
        Response::builder().header("content-type","application/json").header("request-id","req_gateway").header("anthropic-ratelimit-requests-remaining","99").body(Body::from("{\n  \"type\": \"message\", \"id\": \"msg_gateway\", \"content\": [], \"vendor_cost\": 1.2300\n}\n")).expect("response")
    }
}

#[tokio::test]
async fn transcript_preserves_exact_json_headers_and_matches_reordered_keys() {
    let upstream = spawn_router(Router::new().route("/gateway/messages", post(gateway))).await;
    let file = TempFile::new();
    let mut cfg = proxy_config(&upstream, &file, RecordFormat::Transcript, false);
    cfg.upstream_messages_path = Some("/gateway/messages".to_owned());
    let proxy = spawn(cfg).await;
    let req = request(false);
    let recorded = proxy
        .client
        .post(format!("{}/v1/messages", proxy.url))
        .header("x-api-key", "suite")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "test-beta")
        .json(&req)
        .send()
        .await
        .expect("proxy");
    assert_eq!(recorded.status(), 200);
    assert_eq!(
        recorded.headers()["anthropic-ratelimit-requests-remaining"],
        "99"
    );
    let bytes = recorded.bytes().await.expect("body");
    let replay = spawn(Config {
        scenarios_path: Some(file.0.clone()),
        ..config()
    })
    .await;
    let reordered = json!({"stream":false,"messages":[{"content":"hello","role":"user"}],"max_tokens":128,"model":"claude-test"});
    let response = replay.post("/v1/messages", "suite", &reordered).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["request-id"], "req_gateway");
    assert_eq!(response.bytes().await.expect("replay"), bytes);
}

#[tokio::test]
async fn transcript_streams_keep_event_order_and_extension_fields() {
    let upstream = spawn_router(Router::new().route("/v1/messages", post(gateway))).await;
    let file = TempFile::new();
    let proxy = spawn(proxy_config(
        &upstream,
        &file,
        RecordFormat::Transcript,
        false,
    ))
    .await;
    let bytes = proxy
        .post("/v1/messages", "suite", &request(true))
        .await
        .bytes()
        .await
        .expect("body");
    let expected = parse_sse_events(&bytes).expect("events");
    let replay = spawn(Config {
        scenarios_path: Some(file.0.clone()),
        ..config()
    })
    .await;
    let response = replay.post("/v1/messages", "suite", &request(true)).await;
    assert_eq!(response.status(), 200);
    let actual = parse_sse_events(&response.bytes().await.expect("body")).expect("events");
    assert_eq!(
        serde_json::to_value(actual).expect("events"),
        serde_json::to_value(expected).expect("events")
    );
}

#[tokio::test]
async fn append_keeps_namespaces_and_continues_recording_numbers() {
    let upstream = spawn(config()).await;
    let file = TempFile::new();
    for (append, key) in [(false, "a"), (true, "b"), (true, "a")] {
        let proxy = spawn(proxy_config(
            &upstream,
            &file,
            RecordFormat::Transcript,
            append,
        ))
        .await;
        proxy.message(key, &request(false)).await;
    }
    let data = fixture(&file);
    let rows = data["scenarios"].as_array().expect("rows");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["scenario_id"], "a/0001");
    assert_eq!(rows[1]["scenario_id"], "b/0001");
    assert_eq!(rows[2]["scenario_id"], "a/0002");
    let replay = spawn(Config {
        scenarios_path: Some(file.0.clone()),
        ..config()
    })
    .await;
    replay.message("b", &request(false)).await;
    replay.message("a", &request(false)).await;
    replay.message("a", &request(false)).await;
    assert_eq!(
        replay
            .post("/v1/messages", "a", &request(false))
            .await
            .status(),
        400
    );
}

#[tokio::test]
async fn utilities_and_errors_pass_through_without_recording() {
    let upstream = spawn(config()).await;
    let file = TempFile::new();
    let proxy = spawn(proxy_config(
        &upstream,
        &file,
        RecordFormat::Semantic,
        false,
    ))
    .await;
    assert_eq!(proxy.get("/v1/models", "suite").await.status(), 200);
    let req = json!({"model":"claude-test","messages":[{"role":"user","content":"hello"}]});
    assert!(proxy
        .post("/v1/messages/count_tokens", "suite", &req)
        .await
        .json::<Value>()
        .await
        .expect("count")["input_tokens"]
        .is_number());
    upstream.enqueue("upstream-secret",json!([scenario(json!({"kind":"error","status":529,"message":"overloaded","error_type":"overloaded_error","retry_after":"3"}))])).await;
    let error = proxy.post("/v1/messages", "suite", &request(false)).await;
    assert_eq!(error.status(), 529);
    assert_eq!(error.headers()["retry-after"], "3");
    assert_eq!(fixture(&file)["scenarios"], json!([]));
}

#[tokio::test]
async fn malformed_and_interrupted_exchanges_do_not_become_semantic_successes() {
    let upstream=spawn_router(Router::new().route("/v1/messages",post(||async {Response::builder().header("content-type","text/event-stream").body(Body::from("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\"}}\n\n")).expect("body")})).route("/v1/models",get(||async {"models"}))).await;
    let file = TempFile::new();
    let proxy = spawn(proxy_config(
        &upstream,
        &file,
        RecordFormat::Semantic,
        false,
    ))
    .await;
    proxy
        .post("/v1/messages", "suite", &request(true))
        .await
        .bytes()
        .await
        .expect("partial stream");
    assert_eq!(fixture(&file)["scenarios"], json!([]));
}
