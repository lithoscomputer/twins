//! End-to-end round trip for transcript recording, with no network.
//!
//! A bespoke upstream stands in for a non-OpenAI gateway (Venice-style): its
//! bodies carry extension fields the canonical engine does not model — a
//! top-level `cost`, `reasoning_content`, Anthropic-style cache counters, a
//! `stop` finish reason next to tool calls — and its streams carry several
//! deltas. Transcript recording must preserve all of it: replay returns the
//! recorded bytes' JSON verbatim and the recorded SSE events one by one,
//! matched to requests by body hash rather than order.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::{Body, Bytes};
use axum::http::{header, Response as HttpResponse};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use twin_openai::config::{Config, Mode, RecordFormat};

const UPSTREAM_KEY: &str = "upstream-secret";
const BEARER: &str = "transcript-suite";
static NEXT_RECORDING_PATH_ID: AtomicU64 = AtomicU64::new(0);

fn recording_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "twin-openai-transcript-recording-{}-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be sane")
            .as_nanos(),
        NEXT_RECORDING_PATH_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

/// The upstream's non-streaming body: OpenAI-shaped, with the extension
/// fields a real gateway adds. The first user message is echoed so distinct
/// requests get distinct recordings.
fn upstream_json_body(echo: &str) -> Value {
    json!({
        "id": "chatcmpl-upstream",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": format!("echo: {echo}"),
                "reasoning_content": "thought about it",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                }]
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 9,
            "completion_tokens": 3,
            "prompt_tokens_details": { "cache_creation_input_tokens": 7 },
            "cache_creation_input_tokens": 7
        },
        "cost": { "usd": 0.00123, "diem": 0.00123 }
    })
}

/// The upstream's stream: several deltas (a reasoning delta included), a
/// usage chunk with an extension counter, and the terminator.
fn upstream_sse_frames() -> Vec<String> {
    vec![
        json!({ "choices": [{ "index": 0, "delta": { "reasoning_content": "thinking" } }] })
            .to_string(),
        json!({ "choices": [{ "index": 0, "delta": { "content": "Hel" } }] }).to_string(),
        json!({ "choices": [{ "index": 0, "delta": { "content": "lo \u{1F30A}" } }] }).to_string(),
        json!({
            "choices": [],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "cache_creation_input_tokens": 7 }
        })
        .to_string(),
        "[DONE]".to_owned(),
    ]
}

async fn upstream_chat(body: Bytes) -> HttpResponse<Body> {
    let request: Value = serde_json::from_slice(&body).expect("upstream request should be JSON");
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let echo = request
        .pointer("/messages/0/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    if stream {
        let mut sse = String::new();
        for frame in upstream_sse_frames() {
            sse.push_str("data:");
            sse.push_str(&frame);
            sse.push_str("\r\n\r\n");
        }
        HttpResponse::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(sse))
            .expect("upstream stream response should build")
    } else {
        HttpResponse::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(upstream_json_body(&echo).to_string()))
            .expect("upstream response should build")
    }
}

async fn spawn_upstream() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream listener should bind");
    let addr: SocketAddr = listener.local_addr().expect("listener should have addr");
    let app = Router::new().route("/v1/chat/completions", post(upstream_chat));
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("upstream should run");
    });
    format!("http://{addr}")
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

fn proxy_config(upstream_url: &str, recording: &std::path::Path, append: bool) -> Config {
    Config {
        mode: Mode::ProxyRecord,
        upstream_url: upstream_url.to_owned(),
        upstream_responses_path: None,
        upstream_api_key: Some(UPSTREAM_KEY.to_owned()),
        recording_path: Some(recording.to_owned()),
        record_format: RecordFormat::Transcript,
        recording_append: append,
        ..Config::default()
    }
}

fn replay_config(recording: &std::path::Path) -> Config {
    Config {
        scenarios_path: Some(recording.to_owned()),
        allow_unmatched: false,
        ..Config::default()
    }
}

fn client(base_url: &str) -> common::ApiClient {
    common::ApiClient::new(base_url, Some(BEARER.to_owned()), None, None)
        .expect("client should build")
}

fn chat_request(text: &str) -> Value {
    json!({
        "model": "deepseek-v4-flash-0731",
        "messages": [{ "role": "user", "content": text }],
        "stream": false,
        "venice_parameters": { "include_venice_system_prompt": false },
        "prompt_cache_key": "lithos-abc123"
    })
}

fn chat_request_with_reasoning_details(text: &str) -> Value {
    json!({
        "model": "deepseek-v4-flash-0731",
        "messages": [
            { "role": "user", "content": text },
            {
                "role": "assistant",
                "content": null,
                "reasoning_details": [{
                    "type": "reasoning.encrypted",
                    "id": "reasoning-1",
                    "data": "AQ=="
                }],
                "tool_calls": [{
                    "id": "call-weather",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            },
            {
                "role": "tool",
                "content": "21C and sunny",
                "tool_call_id": "call-weather"
            }
        ],
        "stream": false
    })
}

fn chat_stream_request() -> Value {
    json!({
        "model": "deepseek-v4-flash-0731",
        "messages": [{ "role": "user", "content": "Stream a greeting." }],
        "stream": true
    })
}

#[tokio::test]
async fn transcript_replays_json_verbatim_and_matches_by_hash() {
    let upstream = spawn_upstream().await;
    let recording = recording_path();

    // Record two distinct exchanges in one namespace.
    let proxy = spawn(proxy_config(&upstream, &recording, false)).await;
    let proxy_client = client(&proxy);
    let first = chat_request("First question.");
    let second = chat_request_with_reasoning_details("Second question.");
    for request in [&first, &second] {
        let response = proxy_client
            .post_json("/v1/chat/completions", request)
            .await;
        assert_eq!(response.status(), 200);
    }

    // Replay in the opposite order: the body hash picks the right recording
    // regardless of position in the namespace queue.
    let replay = spawn(replay_config(&recording)).await;
    let replay_client = client(&replay);

    let second_replay = replay_client
        .post_json("/v1/chat/completions", &second)
        .await;
    assert_eq!(second_replay.status(), 200);
    let second_body: Value = second_replay
        .json()
        .await
        .expect("replay body should parse");
    assert_eq!(second_body, upstream_json_body("Second question."));

    // Reordering object keys, including inside messages, must not change
    // which recorded request matches.
    let reordered_first = json!({
        "prompt_cache_key": "lithos-abc123",
        "venice_parameters": { "include_venice_system_prompt": false },
        "stream": false,
        "messages": [{ "content": "First question.", "role": "user" }],
        "model": "deepseek-v4-flash-0731"
    });
    let first_replay = replay_client
        .post_json("/v1/chat/completions", &reordered_first)
        .await;
    assert_eq!(first_replay.status(), 200);
    let first_body: Value = first_replay.json().await.expect("replay body should parse");
    assert_eq!(first_body, upstream_json_body("First question."));

    // The extension fields the canonical engine does not model survived.
    assert_eq!(first_body["cost"]["usd"], json!(0.00123));
    assert_eq!(first_body["usage"]["cache_creation_input_tokens"], json!(7));
    assert_eq!(
        first_body["choices"][0]["message"]["reasoning_content"],
        json!("thought about it")
    );
    assert_eq!(first_body["choices"][0]["finish_reason"], json!("stop"));

    let _ = std::fs::remove_file(&recording);
}

#[tokio::test]
async fn transcript_replays_sse_events_one_by_one() {
    let upstream = spawn_upstream().await;
    let recording = recording_path();

    let proxy = spawn(proxy_config(&upstream, &recording, false)).await;
    let request = chat_stream_request();
    let recorded = client(&proxy)
        .post_json("/v1/chat/completions", &request)
        .await;
    assert_eq!(recorded.status(), 200);
    let recorded_bytes = recorded.bytes().await.expect("recorded body should read");

    let replay = spawn(replay_config(&recording)).await;
    let replayed = client(&replay)
        .post_json("/v1/chat/completions", &request)
        .await;
    assert_eq!(replayed.status(), 200);
    assert_eq!(
        replayed
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let replayed_bytes = replayed.bytes().await.expect("replayed body should read");

    // Event for event, the replay is the recording — the deltas stay
    // separate instead of collapsing into one canonical delta.
    let expected_recorded = format!(
        "data:{}\r\n\r\n",
        upstream_sse_frames().join("\r\n\r\ndata:")
    );
    assert_eq!(recorded_bytes.as_ref(), expected_recorded.as_bytes());
    let replayed_transcript =
        common::parse_sse_transcript(&replayed_bytes).expect("replayed SSE should parse");
    assert_eq!(
        replayed_transcript
            .events
            .iter()
            .map(|event| event.data.clone())
            .collect::<Vec<_>>(),
        upstream_sse_frames()
    );
    assert!(replayed_transcript
        .events
        .iter()
        .all(|event| event.event.is_none()));

    let _ = std::fs::remove_file(&recording);
}

#[tokio::test]
async fn an_unrecorded_request_misses_strictly() {
    let upstream = spawn_upstream().await;
    let recording = recording_path();

    let proxy = spawn(proxy_config(&upstream, &recording, false)).await;
    let recorded = client(&proxy)
        .post_json("/v1/chat/completions", &chat_request("Recorded."))
        .await;
    assert_eq!(recorded.status(), 200);

    let replay = spawn(replay_config(&recording)).await;
    let miss = client(&replay)
        .post_json("/v1/chat/completions", &chat_request("Never recorded."))
        .await;
    assert_eq!(miss.status(), 400);
    let body: Value = miss.json().await.expect("miss body should parse");
    assert_eq!(body["error"]["code"], json!("scenario_not_found"));

    let _ = std::fs::remove_file(&recording);
}

#[tokio::test]
async fn append_keeps_earlier_recordings() {
    let upstream = spawn_upstream().await;
    let recording = recording_path();

    let first_proxy = spawn(proxy_config(&upstream, &recording, false)).await;
    let response = client(&first_proxy)
        .post_json("/v1/chat/completions", &chat_request("First run."))
        .await;
    assert_eq!(response.status(), 200);

    let second_proxy = spawn(proxy_config(&upstream, &recording, true)).await;
    let response = client(&second_proxy)
        .post_json("/v1/chat/completions", &chat_request("Second run."))
        .await;
    assert_eq!(response.status(), 200);

    let contents = std::fs::read_to_string(&recording).expect("recording should read");
    let document: Value = serde_json::from_str(&contents).expect("recording should parse");
    let scenarios = document["scenarios"]
        .as_array()
        .expect("recording should hold scenarios");
    assert_eq!(scenarios.len(), 2);
    assert_eq!(scenarios[0]["scenario_id"], json!(format!("{BEARER}/0001")));
    assert_eq!(scenarios[1]["scenario_id"], json!(format!("{BEARER}/0002")));

    // Both replay from the merged file.
    let replay = spawn(replay_config(&recording)).await;
    for text in ["First run.", "Second run."] {
        let response = client(&replay)
            .post_json("/v1/chat/completions", &chat_request(text))
            .await;
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.expect("replay body should parse");
        assert_eq!(body, upstream_json_body(text));
    }

    let _ = std::fs::remove_file(&recording);
}

#[test]
fn the_generic_upstream_key_and_record_format_parse() {
    let lookup = |name: &str| match name {
        "TWIN_OPENAI_UPSTREAM_API_KEY" => Some("generic-key".to_owned()),
        "TWIN_OPENAI_RECORD_FORMAT" => Some("transcript".to_owned()),
        "TWIN_OPENAI_MODE" => Some("proxy-record".to_owned()),
        "TWIN_OPENAI_RECORDING_PATH" => Some("recording.json".to_owned()),
        _ => None,
    };
    let config = Config::from_lookup(&lookup).expect("config should parse");
    assert_eq!(config.upstream_api_key.as_deref(), Some("generic-key"));
    assert_eq!(config.record_format, RecordFormat::Transcript);

    let invalid = |name: &str| (name == "TWIN_OPENAI_RECORD_FORMAT").then(|| "verbatim".to_owned());
    assert!(Config::from_lookup(&invalid).is_err());
}
