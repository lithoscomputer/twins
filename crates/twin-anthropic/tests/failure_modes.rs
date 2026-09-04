mod common;
use common::{config, request, scenario, spawn};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::time::{timeout, Duration, Instant};
use twin_anthropic::record::{message_from_events, parse_sse_events};

#[tokio::test]
async fn error_statuses_retry_headers_and_native_envelopes() {
    let server = spawn(config()).await;
    for (status, kind) in [
        (400, "invalid_request_error"),
        (401, "authentication_error"),
        (403, "permission_error"),
        (404, "not_found_error"),
        (413, "request_too_large"),
        (429, "rate_limit_error"),
        (500, "api_error"),
        (529, "overloaded_error"),
    ] {
        server.enqueue("a",json!([scenario(json!({"kind":"error","status":status,"error_type":kind,"message":"scripted failure","retry_after":"2","headers":{"anthropic-ratelimit-requests-remaining":"0","request-id":"req_test"}}))])).await;
        let response = server.post("/v1/messages", "a", &request(false)).await;
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()["retry-after"], "2");
        assert_eq!(response.headers()["request-id"], "req_test");
        let body: Value = response.json().await.expect("error");
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], kind);
    }
}

#[tokio::test]
async fn delayed_headers_and_hangs_are_observable() {
    let server = spawn(config()).await;
    server
        .enqueue(
            "a",
            json!([
                scenario(json!({"kind":"success","delay_before_headers_ms":60})),
                scenario(json!({"kind":"hang"}))
            ]),
        )
        .await;
    let start = Instant::now();
    server.message("a", &request(false)).await;
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert!(timeout(
        Duration::from_millis(50),
        server.post("/v1/messages", "a", &request(false))
    )
    .await
    .is_err());
}

#[tokio::test]
async fn partial_and_malformed_streams_are_observable() {
    let server = spawn(config()).await;
    for script in [
        json!({"kind":"success","close_after_chunks":2}),
        json!({"kind":"success","malformed_sse":true}),
    ] {
        server.enqueue("a", json!([scenario(script)])).await;
        let bytes = server
            .post("/v1/messages", "a", &request(true))
            .await
            .bytes()
            .await
            .expect("body");
        assert!(message_from_events(&parse_sse_events(&bytes).expect("events")).is_err());
    }
}

#[tokio::test]
async fn stream_event_delays_are_observable() {
    let server = spawn(config()).await;
    server
        .enqueue(
            "a",
            json!([scenario(
                json!({"kind":"success","inter_event_delay_ms":30})
            )]),
        )
        .await;
    let start = Instant::now();
    let response = server.post("/v1/messages", "a", &request(true)).await;
    response.bytes().await.expect("body");
    assert!(start.elapsed() >= Duration::from_millis(120));
}

#[tokio::test]
async fn raw_bytes_headers_and_connection_failures() {
    let server = spawn(config()).await;
    server.enqueue("a",json!([scenario(json!({"kind":"raw","status":201,"headers":{"content-type":"text/plain","x-case":"bytes"},"content_type":"application/octet-stream","chunks":[{"kind":"text","text":"prefix"},{"kind":"bytes","bytes":[0,255,254]}]}))])).await;
    let response = server.post("/v1/messages", "a", &request(false)).await;
    assert_eq!(response.status(), 201);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(response.headers()["x-case"], "bytes");
    assert_eq!(
        response.bytes().await.expect("bytes").as_ref(),
        b"prefix\x00\xff\xfe"
    );
    server.enqueue("a",json!([scenario(json!({"kind":"raw","status":200,"chunks":[{"kind":"text","text":"prefix"},{"kind":"error","message":"connection reset","delay_ms":40},{"kind":"text","text":"never"}]}))])).await;
    let response = server.post("/v1/messages", "a", &request(false)).await;
    let mut stream = response.bytes_stream();
    assert_eq!(
        stream
            .next()
            .await
            .expect("chunk")
            .expect("prefix")
            .as_ref(),
        b"prefix"
    );
    assert!(stream.next().await.expect("body error").is_err());
}

#[tokio::test]
async fn raw_count_scenarios_and_stream_error_events() {
    let server = spawn(config()).await;
    server.enqueue("a",json!([{"matcher":{"endpoint":"messages.count_tokens"},"script":{"kind":"raw","status":200,"content_type":"application/json","chunks":[{"kind":"text","text":"{\"input_tokens\":789}"}]}}])).await;
    let req = json!({"model":"claude-test","messages":[{"role":"user","content":"hello"}]});
    assert_eq!(
        server
            .post("/v1/messages/count_tokens", "a", &req)
            .await
            .json::<Value>()
            .await
            .expect("count")["input_tokens"],
        789
    );
    server.enqueue("a",json!([scenario(json!({"kind":"transcript","status":200,"events":[{"event":"error","data":"{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}"}]}))])).await;
    let bytes = server
        .post("/v1/messages", "a", &request(true))
        .await
        .bytes()
        .await
        .expect("stream");
    let events = parse_sse_events(&bytes).expect("events");
    assert_eq!(events[0].event.as_deref(), Some("error"));
    assert!(message_from_events(&events).is_err());
}
