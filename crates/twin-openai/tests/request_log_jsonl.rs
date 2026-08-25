use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};
use twin_openai::config::Config;
use twin_openai::engine::scenario::RequestContext;
use twin_openai::state::{AppState, NamespaceKey};

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

fn request_log_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "twin-openai-{name}-{}-{}.jsonl",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::SeqCst)
    ))
}

fn config(request_log_path: &Path) -> Config {
    Config {
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000),
        require_auth: true,
        enable_admin: true,
        request_log_path: Some(request_log_path.to_owned()),
        scenarios_path: None,
        allow_unmatched: false,
    }
}

fn request(endpoint: &str, input_text: &str) -> RequestContext {
    RequestContext {
        endpoint: endpoint.to_owned(),
        model: "gpt-test".to_owned(),
        stream: true,
        input_text: input_text.to_owned(),
        instructions_text: String::new(),
        metadata: Map::new(),
    }
}

#[test]
fn request_records_stream_to_jsonl_without_bearer_tokens() {
    let path = request_log_path("records");
    let state = AppState::new(config(&path)).expect("state should open request log");
    let namespace = NamespaceKey::Bearer("super-secret-test-token".to_owned());

    state.log_request(
        &namespace,
        request("responses", "first request"),
        Some("first-scenario".to_owned()),
    );
    state.log_request(
        &namespace,
        request("chat.completions", "second request"),
        None,
    );

    let contents = fs::read_to_string(&path).expect("request log should be readable");
    let records = contents
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("line should contain valid JSON"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["scenario_id"], "first-scenario");
    assert_eq!(records[0]["endpoint"], "responses");
    assert_eq!(records[0]["input_text"], "first request");
    assert_eq!(records[1]["endpoint"], "chat.completions");
    assert_eq!(records[1]["input_text"], "second request");
    assert!(records[1].get("scenario_id").is_none());
    assert!(!contents.contains("super-secret-test-token"));
    assert_eq!(state.request_logs(&namespace).len(), 2);

    drop(state);
    fs::remove_file(path).expect("request log should be removable");
}

#[test]
fn request_log_is_truncated_when_state_starts() {
    let path = request_log_path("truncate");
    fs::write(&path, "stale record\n").expect("stale request log should write");

    let state = AppState::new(config(&path)).expect("state should open request log");
    assert_eq!(
        fs::read_to_string(&path).expect("request log should be readable"),
        ""
    );
    state.log_request(
        &NamespaceKey::Global,
        request("responses", "fresh request"),
        None,
    );
    let contents = fs::read_to_string(&path).expect("request log should be readable");
    assert!(!contents.contains("stale record"));
    assert!(contents.contains("fresh request"));

    drop(state);
    fs::remove_file(path).expect("request log should be removable");
}

#[test]
fn invalid_request_log_path_prevents_state_startup() {
    let parent = request_log_path("invalid-parent");
    fs::write(&parent, "not a directory").expect("parent fixture should write");
    let path = parent.join("requests.jsonl");

    let error = AppState::new(config(&path)).expect_err("invalid request log path should fail");
    assert!(error
        .to_string()
        .contains("failed to create twin-openai request log directory"));

    fs::remove_file(parent).expect("parent fixture should be removable");
}
