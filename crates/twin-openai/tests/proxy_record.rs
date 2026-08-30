//! End-to-end round trip for proxy-record mode, with no network.
//!
//! An ordinary twin plays "live OpenAI" as the upstream. A second server in
//! proxy-record mode forwards to it and records. The recording then seeds a
//! third server in strict fixture mode, and the same requests replay with
//! the recorded content, isolated per bearer-token namespace.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;

use serde_json::{json, Value};
use tokio::net::TcpListener;
use twin_openai::config::{Config, Mode, RecordFormat};

const UPSTREAM_KEY: &str = "upstream-secret";

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
