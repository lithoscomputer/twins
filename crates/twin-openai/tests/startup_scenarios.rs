use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Map;
use twin_openai::config::Config;
use twin_openai::engine::scenario::RequestContext;
use twin_openai::state::{AppState, NamespaceKey};

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

fn scenarios_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "twin-openai-{name}-{}-{}.json",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::SeqCst)
    ))
}

fn config(scenarios_path: PathBuf) -> Config {
    Config {
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000),
        require_auth: true,
        enable_admin: true,
        request_log_path: None,
        scenarios_path: Some(scenarios_path),
    }
}

fn request() -> RequestContext {
    RequestContext {
        endpoint: "responses".to_owned(),
        model: "gpt-test".to_owned(),
        stream: false,
        input_text: "hello".to_owned(),
        instructions_text: String::new(),
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
    let state = AppState::new(config(path.clone())).expect("state should load scenario fixture");
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
fn invalid_startup_scenario_file_prevents_state_startup() {
    let path = scenarios_path("invalid");
    fs::write(&path, "not json").expect("invalid fixture should write");

    let error = AppState::new(config(path.clone())).expect_err("invalid fixture should fail");
    assert!(error
        .to_string()
        .contains("failed to parse twin-openai scenarios"));

    fs::remove_file(path).expect("scenario fixture should be removable");
}
