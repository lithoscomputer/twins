use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Map;
use twin_openai::config::{Config, RecordFormat};
use twin_openai::engine::execute_responses_request;
use twin_openai::engine::scenario::RequestContext;
use twin_openai::openai::models::ResponsesRequest;
use twin_openai::state::{AppState, NamespaceKey};

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

fn scenarios_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "twin-openai-{name}-{}-{}.json",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::SeqCst)
    ))
}

fn config(scenarios_path: PathBuf, allow_unmatched: bool) -> Config {
    Config {
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000),
        require_auth: true,
        enable_admin: true,
        request_log_path: None,
        scenarios_path: Some(scenarios_path),
        allow_unmatched,
        mode: twin_openai::config::Mode::Twin,
        upstream_url: "https://api.openai.com".to_owned(),
        upstream_responses_path: None,
        upstream_api_key: None,
        recording_path: None,
        record_format: RecordFormat::Semantic,
        recording_append: false,
    }
}

fn request() -> RequestContext {
    RequestContext {
        endpoint: "responses".to_owned(),
        model: "gpt-test".to_owned(),
        stream: false,
        input_text: "hello".to_owned(),
        instructions_text: String::new(),
        request_hash: None,
        metadata: Map::new(),
    }
}

#[test]
fn startup_scenarios_are_isolated_per_namespace_and_restored_on_reset() {
    let path = scenarios_path("template");
    fs::write(
        &path,
        r#"{
            "scenarios": [{
                "scenario_id": "startup-response",
                "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
                "script": { "kind": "success", "response_text": "scripted" }
            }]
        }"#,
    )
    .expect("scenario fixture should write");
    let state =
        AppState::new(config(path.clone(), false)).expect("state should load scenario fixture");
    let primary = NamespaceKey::Bearer("primary".to_owned());
    let secondary = NamespaceKey::Bearer("secondary".to_owned());

    assert_eq!(
        state
            .take_matching_scenario(&primary, &request())
            .expect("primary scenario")
            .scenario_id
            .as_deref(),
        Some("startup-response")
    );
    assert_eq!(
        state
            .take_matching_scenario(&secondary, &request())
            .expect("secondary scenario")
            .scenario_id
            .as_deref(),
        Some("startup-response")
    );
    assert!(state.take_matching_scenario(&primary, &request()).is_none());

    state.reset(&primary);
    assert_eq!(
        state
            .take_matching_scenario(&primary, &request())
            .expect("reset should restore primary scenario")
            .scenario_id
            .as_deref(),
        Some("startup-response")
    );
    assert!(state
        .take_matching_scenario(&secondary, &request())
        .is_none());

    drop(state);
    fs::remove_file(path).expect("scenario fixture should be removable");
}

#[test]
fn namespaced_startup_scenarios_seed_only_their_bearer_namespace() {
    let path = scenarios_path("namespaced");
    fs::write(
        &path,
        r#"{
            "scenarios": [
                {
                    "scenario_id": "shared",
                    "matcher": { "endpoint": "responses", "stream": false },
                    "script": { "kind": "success", "response_text": "shared" }
                },
                {
                    "scenario_id": "only-a",
                    "namespace": "test-a",
                    "matcher": { "endpoint": "responses", "stream": false },
                    "script": { "kind": "success", "response_text": "for test-a" }
                }
            ]
        }"#,
    )
    .expect("scenario fixture should write");
    let state = AppState::new(config(path.clone(), false)).expect("state should load fixture");
    let test_a = NamespaceKey::Bearer("test-a".to_owned());
    let test_b = NamespaceKey::Bearer("test-b".to_owned());

    // test-a receives the shared scenario followed by its own.
    assert_eq!(
        state
            .take_matching_scenario(&test_a, &request())
            .expect("first test-a scenario")
            .scenario_id
            .as_deref(),
        Some("shared")
    );
    assert_eq!(
        state
            .take_matching_scenario(&test_a, &request())
            .expect("second test-a scenario")
            .scenario_id
            .as_deref(),
        Some("only-a")
    );
    assert!(state.take_matching_scenario(&test_a, &request()).is_none());

    // test-b receives only the shared scenario.
    assert_eq!(
        state
            .take_matching_scenario(&test_b, &request())
            .expect("test-b scenario")
            .scenario_id
            .as_deref(),
        Some("shared")
    );
    assert!(state.take_matching_scenario(&test_b, &request()).is_none());

    // Reset re-applies the namespace filter.
    state.reset(&test_b);
    assert_eq!(
        state
            .take_matching_scenario(&test_b, &request())
            .expect("reset test-b scenario")
            .scenario_id
            .as_deref(),
        Some("shared")
    );
    assert!(state.take_matching_scenario(&test_b, &request()).is_none());

    drop(state);
    fs::remove_file(path).expect("scenario fixture should be removable");
}

#[test]
fn invalid_startup_scenario_file_prevents_state_startup() {
    let path = scenarios_path("invalid");
    fs::write(&path, "not json").expect("invalid fixture should write");

    let error =
        AppState::new(config(path.clone(), false)).expect_err("invalid fixture should fail");
    assert!(error
        .to_string()
        .contains("failed to parse twin-openai scenarios"));

    fs::remove_file(path).expect("scenario fixture should be removable");
}

#[test]
fn fixture_mode_rejects_unmatched_requests_unless_explicitly_allowed() {
    let path = scenarios_path("strict");
    fs::write(&path, r#"{ "scenarios": [] }"#).expect("scenario fixture should write");
    let namespace = NamespaceKey::Bearer("strict".to_owned());
    let request: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-test",
        "input": "unmatched",
        "stream": false
    }))
    .expect("request should parse");

    let strict_state =
        AppState::new(config(path.clone(), false)).expect("strict state should start");
    let error = execute_responses_request(&strict_state, &namespace, &request, None)
        .expect_err("strict fixture mode should reject unmatched request");
    assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(error.body.error.code, "scenario_not_found");
    assert_eq!(strict_state.request_logs(&namespace).len(), 1);

    let permissive_state =
        AppState::new(config(path.clone(), true)).expect("permissive state should start");
    assert!(execute_responses_request(&permissive_state, &namespace, &request, None).is_ok());

    fs::remove_file(path).expect("scenario fixture should be removable");
}
