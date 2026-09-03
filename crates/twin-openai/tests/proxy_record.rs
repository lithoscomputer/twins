//! End-to-end round trip for proxy-record mode, with no network.
//!
//! An ordinary twin plays "live OpenAI" as the upstream. A second server in
//! proxy-record mode forwards to it and records. The recording then seeds a
//! third server in strict fixture mode, and the same requests replay with
//! the recorded content, isolated per bearer-token namespace.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use twin_openai::config::{Config, Mode, RecordFormat};

const UPSTREAM_KEY: &str = "upstream-secret";

#[derive(Clone, Debug)]
struct CapturedRequest {
    operation: String,
    authorization: Option<String>,
    organization: Option<String>,
    project: Option<String>,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct CaptureState {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

fn recording_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "twin-openai-proxy-recording-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be sane")
            .as_nanos()
    ))
}

async fn spawn(config: Config) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let addr: SocketAddr = listener.local_addr().expect("listener should have addr");
    let app = twin_openai::build_app_with_config(config).expect("app should build");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server should run");
    });
    format!("http://{addr}")
}

async fn spawn_router(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let addr: SocketAddr = listener.local_addr().expect("listener should have addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server should run");
    });
    format!("http://{addr}")
}

fn capture_request(state: &CaptureState, operation: &str, headers: &HeaderMap, body: &Bytes) {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    };
    state
        .requests
        .lock()
        .expect("capture lock should not be poisoned")
        .push(CapturedRequest {
            operation: operation.to_owned(),
            authorization: header("authorization"),
            organization: header("openai-organization"),
            project: header("openai-project"),
            body: body.to_vec(),
        });
}

async fn capture_models(State(state): State<CaptureState>, headers: HeaderMap) -> Json<Value> {
    capture_request(&state, "models", &headers, &Bytes::new());
    Json(json!({
        "object": "list",
        "data": [{
            "id": "upstream-model",
            "object": "model",
            "created": 123,
            "owned_by": "upstream",
            "shutdown_date": null
        }]
    }))
}

async fn capture_input_tokens(
    State(state): State<CaptureState>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<Value> {
    capture_request(&state, "input_tokens", &headers, &body);
    Json(json!({
        "object": "response.input_tokens",
        "input_tokens": 321
    }))
}

fn client(base_url: &str, bearer: &str) -> common::ApiClient {
    common::ApiClient::new(base_url, Some(bearer.to_owned()), None, None)
        .expect("client should build")
}

fn responses_text_request() -> Value {
    json!({
        "model": "gpt-test",
        "input": "Please reply with a greeting.",
        "stream": false
    })
}

fn input_tokens_request() -> Value {
    json!({
        "model": "gpt-test",
        "instructions": "Answer briefly.",
        "input": "Please reply with a greeting."
    })
}

fn chat_stream_request() -> Value {
    json!({
        "model": "gpt-test",
        "messages": [{ "role": "user", "content": "Please reply with a greeting." }],
        "stream": true,
        "stream_options": { "include_usage": true }
    })
}

fn responses_tool_request() -> Value {
    json!({
        "model": "gpt-test",
        "input": "Use the weather tool for Boston.",
        "stream": false,
        "tools": [{
            "type": "function",
            "name": "lookup_weather",
            "description": "Look up the weather",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
                "additionalProperties": false
            },
            "strict": true
        }],
        "tool_choice": { "type": "function", "name": "lookup_weather" }
    })
}

fn responses_structured_request() -> Value {
    json!({
        "model": "gpt-test",
        "input": "Return a short structured answer.",
        "stream": false,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "answer",
                "schema": {
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "ok": { "type": "boolean" }
                    },
                    "required": ["message", "ok"],
                    "additionalProperties": false
                },
                "strict": true
            }
        }
    })
}

fn response_text(body: &Value) -> Option<String> {
    body.get("output")?.as_array()?.iter().find_map(|item| {
        (item.get("type")?.as_str()? == "message")
            .then(|| {
                item.get("content")?.as_array()?.iter().find_map(|part| {
                    (part.get("type")?.as_str()? == "output_text")
                        .then(|| part.get("text")?.as_str().map(ToOwned::to_owned))
                        .flatten()
                })
            })
            .flatten()
    })
}

fn response_tool_call(body: &Value) -> Option<(String, Value)> {
    body.get("output")?.as_array()?.iter().find_map(|item| {
        (item.get("type")?.as_str()? == "function_call")
            .then(|| {
                let name = item.get("name")?.as_str()?.to_owned();
                let arguments: Value =
                    serde_json::from_str(item.get("arguments")?.as_str()?).ok()?;
                Some((name, arguments))
            })
            .flatten()
    })
}

fn response_structured(body: &Value) -> Option<Value> {
    body.get("output")?.as_array()?.iter().find_map(|item| {
        (item.get("type")?.as_str()? == "message")
            .then(|| {
                item.get("content")?.as_array()?.iter().find_map(|part| {
                    match part.get("type")?.as_str()? {
                        "output_json" => part.get("json").cloned(),
                        "output_text" => serde_json::from_str(part.get("text")?.as_str()?).ok(),
                        _ => None,
                    }
                })
            })
            .flatten()
    })
}

fn chat_stream_content(transcript: &common::ParsedSseTranscript) -> String {
    let mut content = String::new();
    for event in &transcript.events {
        if event.data == "[DONE]" {
            continue;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        if let Some(text) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
        {
            content.push_str(text);
        }
    }
    content
}

#[tokio::test]
async fn proxy_records_and_replays_per_namespace() {
    // "Live OpenAI": an ordinary twin serving deterministic fallbacks, plus
    // one scripted tool scenario enqueued under the upstream key's namespace.
    let upstream = common::spawn_server().await.expect("upstream should spawn");
    let upstream_admin = client(&upstream.base_url, UPSTREAM_KEY);
    let enqueue = upstream_admin
        .post_json(
            "/__admin/scenarios",
            &json!({
                "scenarios": [{
                    "scenario_id": "scripted-tool",
                    "matcher": { "endpoint": "responses", "stream": false, "input_contains": "weather" },
                    "script": {
                        "kind": "success",
                        "tool_calls": [{ "name": "lookup_weather", "arguments": { "city": "Boston" } }],
                        "usage": { "input_tokens": 9, "output_tokens": 4 }
                    }
                }]
            }),
        )
        .await;
    assert_eq!(enqueue.status(), 200);

    // The proxy, recording to a temp file.
    let recording = recording_path();
    let proxy_url = spawn(Config {
        mode: Mode::ProxyRecord,
        upstream_url: upstream.base_url.clone(),
        upstream_responses_path: None,
        upstream_api_key: Some(UPSTREAM_KEY.to_owned()),
        recording_path: Some(recording.clone()),
        record_format: RecordFormat::Semantic,
        recording_append: false,
        ..common::test_config()
    })
    .await;

    // Drive two "E2E tests" through the proxy, each with its own bearer.
    let test_a = client(&proxy_url, "e2e-a");
    let via_proxy_models = test_a
        .get("/v1/models")
        .send()
        .await
        .expect("proxied models call should complete");
    assert_eq!(via_proxy_models.status(), 200);
    let proxied_models: Value = via_proxy_models
        .json()
        .await
        .expect("proxied models body should parse");
    assert_eq!(proxied_models["object"], "list");
    assert_eq!(proxied_models["data"][0]["id"], "gpt-test");

    let via_proxy_tokens = common::post_json_exchange(
        &test_a,
        "/v1/responses/input_tokens",
        &input_tokens_request(),
    )
    .await
    .expect("proxied input-token call should succeed");
    assert_eq!(via_proxy_tokens.body["object"], "response.input_tokens");
    assert!(via_proxy_tokens.body["input_tokens"]
        .as_u64()
        .is_some_and(|count| count > 0));
    let proxied_tokens = via_proxy_tokens.body;

    let via_proxy_text =
        common::post_json_exchange(&test_a, "/v1/responses", &responses_text_request())
            .await
            .expect("proxied responses call should succeed");
    let proxied_text = response_text(&via_proxy_text.body).expect("proxied text");
    assert_eq!(proxied_text, "deterministic: Please reply with a greeting.");

    let via_proxy_chat =
        common::post_sse_exchange(&test_a, "/v1/chat/completions", &chat_stream_request())
            .await
            .expect("proxied chat stream should succeed");
    let proxied_chat = chat_stream_content(&via_proxy_chat.transcript);
    assert_eq!(proxied_chat, "deterministic: Please reply with a greeting.");
    assert!(
        via_proxy_chat.transcript.done,
        "chat stream should end with [DONE]"
    );

    let via_proxy_tool =
        common::post_json_exchange(&test_a, "/v1/responses", &responses_tool_request())
            .await
            .expect("proxied tool call should succeed");
    let proxied_tool = response_tool_call(&via_proxy_tool.body).expect("proxied tool call");
    assert_eq!(proxied_tool.0, "lookup_weather");
    assert_eq!(proxied_tool.1, json!({ "city": "Boston" }));

    let test_b = client(&proxy_url, "e2e-b");
    let via_proxy_structured =
        common::post_json_exchange(&test_b, "/v1/responses", &responses_structured_request())
            .await
            .expect("proxied structured call should succeed");
    let proxied_structured =
        response_structured(&via_proxy_structured.body).expect("proxied structured output");
    assert_eq!(proxied_structured.get("ok"), Some(&json!(true)));

    // The recording holds ordered, namespaced scenarios.
    let recorded: Value =
        serde_json::from_str(&std::fs::read_to_string(&recording).expect("recording should exist"))
            .expect("recording should parse");
    let scenarios = recorded["scenarios"].as_array().expect("scenarios array");
    let ids: Vec<&str> = scenarios
        .iter()
        .map(|scenario| scenario["scenario_id"].as_str().expect("scenario id"))
        .collect();
    assert_eq!(
        ids,
        ["e2e-a/0001", "e2e-a/0002", "e2e-a/0003", "e2e-b/0001"]
    );
    assert!(scenarios
        .iter()
        .all(|scenario| scenario["namespace"].is_string()));

    // Replay the recording in strict fixture mode.
    let replay_url = spawn(Config {
        scenarios_path: Some(recording.clone()),
        ..common::test_config()
    })
    .await;

    let replay_a = client(&replay_url, "e2e-a");
    let replayed_models: Value = replay_a
        .get("/v1/models")
        .send()
        .await
        .expect("replayed models call should complete")
        .json()
        .await
        .expect("replayed models body should parse");
    assert_eq!(replayed_models, proxied_models);

    let replayed_tokens = common::post_json_exchange(
        &replay_a,
        "/v1/responses/input_tokens",
        &input_tokens_request(),
    )
    .await
    .expect("replayed input-token call should succeed");
    assert_eq!(replayed_tokens.body, proxied_tokens);

    let replayed_text =
        common::post_json_exchange(&replay_a, "/v1/responses", &responses_text_request())
            .await
            .expect("replayed responses call should succeed");
    assert_eq!(
        response_text(&replayed_text.body).expect("replayed text"),
        proxied_text
    );

    let replayed_chat =
        common::post_sse_exchange(&replay_a, "/v1/chat/completions", &chat_stream_request())
            .await
            .expect("replayed chat stream should succeed");
    assert_eq!(chat_stream_content(&replayed_chat.transcript), proxied_chat);

    let replayed_tool =
        common::post_json_exchange(&replay_a, "/v1/responses", &responses_tool_request())
            .await
            .expect("replayed tool call should succeed");
    assert_eq!(
        response_tool_call(&replayed_tool.body).expect("replayed tool call"),
        proxied_tool
    );

    // e2e-a's queue is exhausted: a fourth call fails in strict mode.
    let exhausted = replay_a
        .post_json("/v1/responses", &responses_text_request())
        .await;
    assert_eq!(exhausted.status(), 400);
    let exhausted_body: Value = exhausted.json().await.expect("error body should parse");
    assert_eq!(exhausted_body["error"]["code"], "scenario_not_found");

    // e2e-b replays only its own recording.
    let replay_b = client(&replay_url, "e2e-b");
    let replayed_structured =
        common::post_json_exchange(&replay_b, "/v1/responses", &responses_structured_request())
            .await
            .expect("replayed structured call should succeed");
    assert_eq!(
        response_structured(&replayed_structured.body).expect("replayed structured output"),
        proxied_structured
    );

    // An unrecorded bearer gets nothing.
    let replay_c = client(&replay_url, "e2e-c");
    let unmatched = replay_c
        .post_json("/v1/responses", &responses_text_request())
        .await;
    assert_eq!(unmatched.status(), 400);

    std::fs::remove_file(recording).expect("recording should be removable");
}

/// The Codex deployment hangs its Responses endpoint off an unversioned
/// path and authenticates a seat with `ChatGPT-Account-Id` and `originator`
/// headers. The proxy rebases `/v1/responses` onto the configured upstream
/// path, forwards the seat headers, and the recorded exchange replays
/// through the ordinary strict twin at the standard path.
#[tokio::test]
async fn proxy_rebases_the_responses_path_and_forwards_seat_headers() {
    use std::sync::{Arc, Mutex};

    use axum::extract::State as AxumState;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};

    // "Live Codex": a hand-rolled upstream serving only the unversioned
    // deployment path, capturing the headers each request carried.
    #[derive(Clone, Default)]
    struct SeenHeaders {
        authorization: Option<String>,
        account_id: Option<String>,
        originator: Option<String>,
    }

    #[derive(Clone, Default)]
    struct Seen(Arc<Mutex<Vec<SeenHeaders>>>);

    async fn codex_upstream(
        AxumState(seen): AxumState<Seen>,
        headers: HeaderMap,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
        };
        seen.0
            .lock()
            .expect("lock should be sane")
            .push(SeenHeaders {
                authorization: header("authorization"),
                account_id: header("chatgpt-account-id"),
                originator: header("originator"),
            });
        Json(json!({
            "id": "resp_codex",
            "object": "response",
            "status": "completed",
            "model": "gpt-test",
            "output": [{
                "type": "message",
                "id": "msg_codex",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Hello from the seat." }]
            }],
            "usage": {
                "input_tokens": 7,
                "output_tokens": 5,
                "total_tokens": 12
            }
        }))
    }

    let seen = Seen::default();
    let upstream_app = Router::new()
        .route("/backend-api/codex/responses", post(codex_upstream))
        .with_state(seen.clone());
    let upstream_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream listener should bind");
    let upstream_addr = upstream_listener
        .local_addr()
        .expect("upstream listener should have addr");
    tokio::spawn(async move {
        axum::serve(upstream_listener, upstream_app)
            .await
            .expect("upstream should run");
    });

    let recording = recording_path();
    let proxy_url = spawn(Config {
        mode: Mode::ProxyRecord,
        upstream_url: format!("http://{upstream_addr}"),
        upstream_responses_path: Some("/backend-api/codex/responses".to_owned()),
        upstream_api_key: Some(UPSTREAM_KEY.to_owned()),
        recording_path: Some(recording.clone()),
        record_format: RecordFormat::Transcript,
        enable_admin: false,
        ..Config::default()
    })
    .await;

    // The client hits the proxy at the standard `/v1/responses` route; only
    // the upstream half of the exchange is rebased.
    let response = reqwest::Client::new()
        .post(format!("{proxy_url}/v1/responses"))
        .bearer_auth("codex-namespace")
        .header("ChatGPT-Account-Id", "acct_123")
        .header("originator", "twin-test")
        .json(&responses_text_request())
        .send()
        .await
        .expect("proxied request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("body should be JSON");
    assert_eq!(
        response_text(&body).as_deref(),
        Some("Hello from the seat.")
    );

    let captured = seen.0.lock().expect("lock should be sane").clone();
    assert_eq!(captured.len(), 1, "the upstream should see one request");
    assert_eq!(
        captured[0].authorization.as_deref(),
        Some(format!("Bearer {UPSTREAM_KEY}").as_str()),
        "the client bearer must be replaced with the upstream key"
    );
    assert_eq!(captured[0].account_id.as_deref(), Some("acct_123"));
    assert_eq!(captured[0].originator.as_deref(), Some("twin-test"));

    // The recording replays through the strict twin with no upstream at all.
    let replay_url = spawn(Config {
        scenarios_path: Some(recording.clone()),
        allow_unmatched: false,
        enable_admin: false,
        ..Config::default()
    })
    .await;
    let replayed: Value = client(&replay_url, "codex-namespace")
        .post_json("/v1/responses", &responses_text_request())
        .await
        .json()
        .await
        .expect("replayed body should be JSON");
    assert_eq!(
        response_text(&replayed).as_deref(),
        Some("Hello from the seat.")
    );

    std::fs::remove_file(&recording).ok();
}

/// The Codex deployment answers a streaming request with no content-type
/// header at all. The proxy must classify the exchange by the request's own
/// stream flag, record the SSE transcript, and replay it — the JSON path
/// would silently parse-fail and record nothing.
#[tokio::test]
async fn proxy_records_a_stream_served_without_a_content_type() {
    use axum::body::Body;
    use axum::http::Response as HttpResponse;
    use axum::routing::post;
    use axum::Router;

    const SSE_BODY: &str = "event: response.created\n\
        data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n\
        event: response.output_item.done\n\
        data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Bare stream.\"}]}}\n\n\
        event: response.completed\n\
        data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[]}}\n\n";

    async fn bare_upstream() -> HttpResponse<Body> {
        // Deliberately no content-type header, matching the live deployment.
        HttpResponse::new(Body::from(SSE_BODY))
    }

    let upstream_app = Router::new().route("/responses", post(bare_upstream));
    let upstream_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream listener should bind");
    let upstream_addr = upstream_listener
        .local_addr()
        .expect("upstream listener should have addr");
    tokio::spawn(async move {
        axum::serve(upstream_listener, upstream_app)
            .await
            .expect("upstream should run");
    });

    let recording = recording_path();
    let proxy_url = spawn(Config {
        mode: Mode::ProxyRecord,
        upstream_url: format!("http://{upstream_addr}"),
        upstream_responses_path: Some("/responses".to_owned()),
        upstream_api_key: Some(UPSTREAM_KEY.to_owned()),
        recording_path: Some(recording.clone()),
        record_format: RecordFormat::Transcript,
        enable_admin: false,
        ..Config::default()
    })
    .await;

    let request = json!({
        "model": "gpt-test",
        "input": "Please reply with a greeting.",
        "stream": true
    });
    let response = client(&proxy_url, "bare-namespace")
        .post_json("/v1/responses", &request)
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.expect("the stream should read");
    assert!(
        body.contains("Bare stream."),
        "the passthrough must still carry the stream: {body}"
    );

    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(&recording).expect("the recording should exist"),
    )
    .expect("the recording should be JSON");
    let scenarios = recorded["scenarios"]
        .as_array()
        .expect("the recording should hold scenarios");
    assert_eq!(
        scenarios.len(),
        1,
        "the bare-header stream must be recorded: {recorded}"
    );
    assert_eq!(scenarios[0]["matcher"]["stream"], json!(true));

    // And the recorded transcript replays through the strict twin.
    let replay_url = spawn(Config {
        scenarios_path: Some(recording.clone()),
        allow_unmatched: false,
        enable_admin: false,
        ..Config::default()
    })
    .await;
    let response = client(&replay_url, "bare-namespace")
        .post_json("/v1/responses", &request)
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.expect("the replay should read");
    assert!(
        body.contains("Bare stream."),
        "the replay must carry the recorded stream: {body}"
    );

    std::fs::remove_file(&recording).ok();
}

#[tokio::test]
async fn utility_proxy_forwards_credentials_headers_and_body() {
    let captures = CaptureState::default();
    let upstream_url = spawn_router(
        Router::new()
            .route("/v1/models", get(capture_models))
            .route("/v1/responses/input_tokens", post(capture_input_tokens))
            .with_state(captures.clone()),
    )
    .await;
    let recording = recording_path();
    let proxy_url = spawn(Config {
        mode: Mode::ProxyRecord,
        upstream_url,
        upstream_api_key: Some(UPSTREAM_KEY.to_owned()),
        recording_path: Some(recording.clone()),
        ..common::test_config()
    })
    .await;
    let client = common::ApiClient::new(
        proxy_url,
        Some("client-namespace".to_owned()),
        Some("org-test".to_owned()),
        Some("project-test".to_owned()),
    )
    .expect("client should build");

    let models = client
        .get("/v1/models")
        .send()
        .await
        .expect("models request should complete");
    assert_eq!(models.status(), 200);
    let models: Value = models.json().await.expect("models body should parse");
    assert_eq!(models["data"][0]["id"], "upstream-model");

    let token_request = input_tokens_request();
    let tokens = client
        .post_json("/v1/responses/input_tokens", &token_request)
        .await;
    assert_eq!(tokens.status(), 200);
    let tokens: Value = tokens.json().await.expect("token body should parse");
    assert_eq!(tokens["input_tokens"], 321);

    let requests = captures
        .requests
        .lock()
        .expect("capture lock should not be poisoned");
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer upstream-secret")
        );
        assert_eq!(request.organization.as_deref(), Some("org-test"));
        assert_eq!(request.project.as_deref(), Some("project-test"));
    }
    assert_eq!(requests[0].operation, "models");
    assert!(requests[0].body.is_empty());
    assert_eq!(requests[1].operation, "input_tokens");
    let forwarded_body: Value =
        serde_json::from_slice(&requests[1].body).expect("forwarded body should be JSON");
    assert_eq!(forwarded_body, token_request);
    drop(requests);

    let recorded_file: Value =
        serde_json::from_str(&std::fs::read_to_string(&recording).expect("recording should exist"))
            .expect("recording should parse");
    assert_eq!(recorded_file["scenarios"], json!([]));
    std::fs::remove_file(recording).expect("recording should be removable");
}
