//! Record-and-replay round trip for the Vercel AI Gateway's evaluation-model
//! API, with no network.
//!
//! A hand-rolled upstream serves a live-captured `POST /v4/ai/evaluation-model`
//! body and captures what the proxy forwarded. The proxy must pass the body
//! and the gateway's `ai-*` headers through under the upstream key, return
//! the upstream answer unchanged, and record one `evaluation` transcript
//! matched by request hash. That recording then replays through the strict
//! twin by hash, in per-namespace order.

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Response as HttpResponse, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use twin_openai::config::{Config, Mode, RecordFormat};
use twin_openai::engine::scenario::ScenarioEnvelope;
use twin_openai::state::{AppState, NamespaceKey};

const PATH: &str = "/v4/ai/evaluation-model";
const UPSTREAM_KEY: &str = "upstream-secret";
const BEARER: &str = "evaluation-suite";
static NEXT_RECORDING_PATH_ID: AtomicU64 = AtomicU64::new(0);

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/evaluation")
        .join(name);
    serde_json::from_slice(&std::fs::read(&path).expect("fixture should read"))
        .expect("fixture should be JSON")
}

fn live_answer() -> Value {
    fixture("jev-live.json")
}

fn mismatch_error() -> Value {
    fixture("jev-mismatch-400.json")
}

fn recording_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "twin-openai-evaluation-recording-{}-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be sane")
            .as_nanos(),
        NEXT_RECORDING_PATH_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    headers: HeaderMap,
    body: Vec<u8>,
}

/// "Live gateway": answers with the captured 200 body, or the captured 400
/// when the model header names a language model.
#[derive(Clone, Default)]
struct Upstream {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

async fn upstream_evaluation(
    State(upstream): State<Upstream>,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse<Body> {
    let mismatch = headers
        .get("ai-model-id")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|model| model.starts_with("anthropic/"));
    upstream
        .requests
        .lock()
        .expect("capture lock should not be poisoned")
        .push(CapturedRequest {
            headers,
            body: body.to_vec(),
        });
    let (status, answer) = if mismatch {
        (StatusCode::BAD_REQUEST, mismatch_error())
    } else {
        (StatusCode::OK, live_answer())
    };
    HttpResponse::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(answer.to_string()))
        .expect("upstream response should build")
}

async fn spawn_upstream() -> (String, Upstream) {
    let upstream = Upstream::default();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream listener should bind");
    let addr: SocketAddr = listener.local_addr().expect("listener should have addr");
    let app = Router::new()
        .route(PATH, post(upstream_evaluation))
        .with_state(upstream.clone());
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("upstream should run");
    });
    (format!("http://{addr}"), upstream)
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

fn proxy_config(upstream_url: &str, recording: &Path) -> Config {
    Config {
        mode: Mode::ProxyRecord,
        upstream_url: upstream_url.to_owned(),
        upstream_api_key: Some(UPSTREAM_KEY.to_owned()),
        recording_path: Some(recording.to_owned()),
        record_format: RecordFormat::Transcript,
        ..common::test_config()
    }
}

fn replay_config(recording: &Path) -> Config {
    Config {
        scenarios_path: Some(recording.to_owned()),
        allow_unmatched: false,
        enable_admin: false,
        ..common::test_config()
    }
}

fn evaluation_request(ticket: &str) -> Value {
    json!({
        "state": { "ticket": ticket },
        "questions": {
            "department": { "type": "choice", "choices": ["billing", "technical", "other"] },
            "severity": { "type": "score", "min": 0, "max": 2 },
            "requests_refund": { "type": "boolean" }
        },
        "providerOptions": { "typesafe": { "confidence": true } }
    })
}

/// Sends an evaluation request the way the gateway client does: the bearer
/// names the namespace, and the three `ai-*` headers ride along.
async fn post_evaluation(
    base_url: &str,
    bearer: &str,
    model: &str,
    body: &Value,
) -> reqwest::Response {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client should build")
        .post(format!("{base_url}{PATH}"))
        .bearer_auth(bearer)
        .header("ai-gateway-protocol-version", "0.0.1")
        .header("ai-evaluation-model-specification-version", "4")
        .header("ai-model-id", model)
        .json(body)
        .send()
        .await
        .expect("request should complete")
}

fn recorded_scenarios(recording: &Path) -> Vec<Value> {
    let document: Value =
        serde_json::from_str(&std::fs::read_to_string(recording).expect("recording should read"))
            .expect("recording should parse");
    document["scenarios"]
        .as_array()
        .expect("recording should hold scenarios")
        .clone()
}

#[tokio::test]
async fn proxy_forwards_the_exchange_and_records_a_hashed_transcript() {
    let (upstream_url, upstream) = spawn_upstream().await;
    let recording = recording_path();
    let proxy = spawn(proxy_config(&upstream_url, &recording)).await;

    let request = evaluation_request("My invoice is wrong, refund me.");
    let response = post_evaluation(&proxy, BEARER, "typesafe-ai/jev", &request).await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("body should be JSON");
    assert_eq!(body, live_answer());

    let captured = upstream
        .requests
        .lock()
        .expect("capture lock should not be poisoned")
        .clone();
    assert_eq!(captured.len(), 1, "the upstream should see one request");
    let forwarded = &captured[0];
    let header = |name: &str| {
        forwarded
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    assert_eq!(
        header("authorization"),
        Some(format!("Bearer {UPSTREAM_KEY}").as_str()),
        "the client bearer must be replaced with the upstream key"
    );
    assert_eq!(header("ai-gateway-protocol-version"), Some("0.0.1"));
    assert_eq!(
        header("ai-evaluation-model-specification-version"),
        Some("4")
    );
    assert_eq!(header("ai-model-id"), Some("typesafe-ai/jev"));
    let forwarded_body: Value =
        serde_json::from_slice(&forwarded.body).expect("forwarded body should be JSON");
    assert_eq!(forwarded_body, request);

    let scenarios = recorded_scenarios(&recording);
    assert_eq!(scenarios.len(), 1);
    let scenario = &scenarios[0];
    assert_eq!(scenario["scenario_id"], json!(format!("{BEARER}/0001")));
    assert_eq!(scenario["namespace"], json!(BEARER));
    assert_eq!(scenario["matcher"]["endpoint"], json!("evaluation"));
    assert_eq!(scenario["matcher"]["stream"], json!(false));
    let expected_hash = twin_openai::record::request_hash(request.to_string().as_bytes())
        .expect("request should hash");
    assert_eq!(scenario["matcher"]["request_hash"], json!(expected_hash));
    assert_eq!(scenario["script"]["kind"], json!("transcript"));
    assert_eq!(scenario["script"]["status"], json!(200));
    assert_eq!(scenario["script"]["body"], live_answer());

    let _ = std::fs::remove_file(&recording);
}

#[tokio::test]
async fn proxy_passes_an_upstream_error_through_unrecorded() {
    let (upstream_url, _upstream) = spawn_upstream().await;
    let recording = recording_path();
    let proxy = spawn(proxy_config(&upstream_url, &recording)).await;

    let response = post_evaluation(
        &proxy,
        BEARER,
        "anthropic/claude-haiku-4.5",
        &evaluation_request("Wrong model."),
    )
    .await;
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.expect("body should be JSON");
    assert_eq!(body, mismatch_error());

    assert!(recorded_scenarios(&recording).is_empty());

    let _ = std::fs::remove_file(&recording);
}

/// The canonical engine has no plan for an evaluation answer, so semantic
/// recording cannot derive one: the exchange passes through and the
/// recording stays empty.
#[tokio::test]
async fn semantic_recording_passes_the_exchange_through_unrecorded() {
    let (upstream_url, _upstream) = spawn_upstream().await;
    let recording = recording_path();
    let proxy = spawn(Config {
        record_format: RecordFormat::Semantic,
        ..proxy_config(&upstream_url, &recording)
    })
    .await;

    let response = post_evaluation(
        &proxy,
        BEARER,
        "typesafe-ai/jev",
        &evaluation_request("Semantic run."),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("body should be JSON");
    assert_eq!(body, live_answer());

    assert!(recorded_scenarios(&recording).is_empty());

    let _ = std::fs::remove_file(&recording);
}

#[tokio::test]
async fn replay_serves_the_recorded_answer_by_hash_and_in_namespace_order() {
    let (upstream_url, _upstream) = spawn_upstream().await;
    let recording = recording_path();
    let proxy = spawn(proxy_config(&upstream_url, &recording)).await;

    // Two exchanges in one namespace, plus one in another.
    let first = evaluation_request("First ticket.");
    let second = evaluation_request("Second ticket.");
    for request in [&first, &second] {
        let response = post_evaluation(&proxy, BEARER, "typesafe-ai/jev", request).await;
        assert_eq!(response.status(), 200);
    }
    let other = post_evaluation(&proxy, "other-suite", "typesafe-ai/jev", &first).await;
    assert_eq!(other.status(), 200);
    assert_eq!(recorded_scenarios(&recording).len(), 3);

    let replay = spawn(replay_config(&recording)).await;

    // The same body under the same bearer replays the recorded answer, with
    // the request's key order left to the client.
    let reordered_first = json!({
        "providerOptions": { "typesafe": { "confidence": true } },
        "questions": first["questions"].clone(),
        "state": { "ticket": "First ticket." }
    });
    let replayed = post_evaluation(&replay, BEARER, "typesafe-ai/jev", &reordered_first).await;
    assert_eq!(replayed.status(), 200);
    assert_eq!(
        replayed
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let body: Value = replayed.json().await.expect("replay body should parse");
    assert_eq!(body, live_answer());
    assert_eq!(
        body["providerMetadata"]["gateway"]["cost"],
        json!("0.000016338")
    );

    // A body that was never recorded misses strictly.
    let miss = post_evaluation(
        &replay,
        BEARER,
        "typesafe-ai/jev",
        &evaluation_request("Never recorded."),
    )
    .await;
    assert_eq!(miss.status(), 400);
    let miss_body: Value = miss.json().await.expect("miss body should parse");
    assert_eq!(miss_body["error"]["code"], json!("scenario_not_found"));

    // The second recorded exchange is still queued and replays next; the
    // queue is then exhausted.
    let replayed = post_evaluation(&replay, BEARER, "typesafe-ai/jev", &second).await;
    assert_eq!(replayed.status(), 200);
    let exhausted = post_evaluation(&replay, BEARER, "typesafe-ai/jev", &second).await;
    assert_eq!(exhausted.status(), 400);

    // The other namespace replays only its own recording.
    let other = post_evaluation(&replay, "other-suite", "typesafe-ai/jev", &first).await;
    assert_eq!(other.status(), 200);
    let unrecorded = post_evaluation(&replay, "unrecorded-suite", "typesafe-ai/jev", &first).await;
    assert_eq!(unrecorded.status(), 400);

    let _ = std::fs::remove_file(&recording);
}

#[tokio::test]
async fn replay_requires_a_bearer_and_a_json_body() {
    let replay = spawn(common::test_config()).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client should build");

    let unauthenticated = client
        .post(format!("{replay}{PATH}"))
        .json(&evaluation_request("No bearer."))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(unauthenticated.status(), 401);
    let body: Value = unauthenticated.json().await.expect("body should parse");
    assert_eq!(body["error"]["code"], json!("missing_bearer_token"));

    let not_json = client
        .post(format!("{replay}{PATH}"))
        .bearer_auth(BEARER)
        .header(header::CONTENT_TYPE, "application/json")
        .body("not json")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(not_json.status(), 400);
    let body: Value = not_json.json().await.expect("body should parse");
    assert_eq!(body["error"]["code"], json!("invalid_request"));
}

#[test]
fn an_evaluation_scenario_passes_validation() {
    let state = AppState::new(common::test_config()).expect("state");
    let envelope: ScenarioEnvelope = serde_json::from_value(json!({
        "scenarios": [{
            "scenario_id": "evaluation-answer",
            "matcher": {
                "endpoint": "evaluation",
                "stream": false,
                "request_hash": "0123456789abcdef"
            },
            "script": {
                "kind": "transcript",
                "status": 200,
                "content_type": "application/json",
                "body": live_answer()
            }
        }]
    }))
    .expect("valid scenario syntax");
    state
        .enqueue_scenarios(&NamespaceKey::Global, envelope.scenarios)
        .expect("an evaluation scenario should be accepted");
    let snapshot = state.debug_snapshot();
    assert_eq!(snapshot.namespaces[0].scenarios[0].endpoint, "evaluation");
}
