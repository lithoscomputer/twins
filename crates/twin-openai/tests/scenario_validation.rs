use serde_json::{json, Value};
use twin_openai::config::{Config, Mode};
use twin_openai::engine::scenario::ScenarioEnvelope;
use twin_openai::state::{AppState, NamespaceKey};

fn scenario(script: Value) -> Value {
    let mut row = json!({"matcher": {"endpoint": "responses"}});
    row["script"] = script;
    row
}

fn invalid_scenarios() -> Vec<Value> {
    let mut invalid = Vec::new();
    for sticky in [false, true] {
        let mut row = scenario(json!({"kind": "success"}));
        row["repeat"] = json!(0);
        row["sticky"] = json!(sticky);
        invalid.push(row);
    }
    for status in [100, 199, 1000] {
        invalid.extend([
            scenario(json!({"kind": "raw", "status": status, "chunks": []})),
            scenario(json!({"kind": "transcript", "status": status, "body": {}})),
            scenario(json!({"kind": "error", "status": status, "message": "bad", "error_type": "api_error", "code": "bad"})),
        ]);
    }
    invalid.extend([
        scenario(json!({"kind": "raw", "status": 200, "chunks": [], "headers": {"bad name": "ok"}})),
        scenario(json!({"kind": "raw", "status": 200, "chunks": [], "headers": {"x-test": "bad\nvalue"}})),
        scenario(json!({"kind": "raw", "status": 200, "chunks": [], "content_type": "bad\nvalue"})),
        scenario(json!({"kind": "transcript", "status": 200, "body": {}, "content_type": "bad\nvalue"})),
        scenario(json!({"kind": "error", "status": 429, "message": "retry", "error_type": "rate_limit_error", "code": "retry", "retry_after": "bad\nvalue"})),
    ]);
    invalid
}

#[test]
fn invalid_scenarios_reject_the_whole_batch() {
    let state = AppState::new(Config::from_lookup(&|_| None).expect("config")).expect("state");
    let namespace = NamespaceKey::Global;
    let good = scenario(json!({"kind": "success", "response_text": "queued"}));
    for bad in invalid_scenarios() {
        let envelope: ScenarioEnvelope = serde_json::from_value(json!({
            "scenarios": [good.clone(), bad.clone()]
        }))
        .expect("valid scenario syntax");
        assert!(
            state
                .enqueue_scenarios(&namespace, envelope.scenarios)
                .is_err(),
            "accepted invalid scenario: {bad}"
        );
        assert!(state.debug_snapshot().namespaces[0].scenarios.is_empty());
    }
}

#[tokio::test]
async fn invalid_scenarios_prevent_startup_and_append_without_changing_the_file() {
    let path = std::env::temp_dir().join(format!(
        "twin-openai-invalid-scenarios-{}.json",
        std::process::id()
    ));
    let base = Config::from_lookup(&|_| None).expect("config");
    for bad in invalid_scenarios() {
        let bytes = serde_json::to_vec(&json!({"scenarios": [bad.clone()]})).expect("JSON");
        std::fs::write(&path, &bytes).expect("scenario file");
        assert!(
            twin_openai::build_app_with_config(Config {
                scenarios_path: Some(path.clone()),
                ..base.clone()
            })
            .is_err(),
            "startup accepted {bad}"
        );
        assert!(
            twin_openai::build_app_with_config(Config {
                mode: Mode::ProxyRecord,
                upstream_api_key: Some("test-key".to_owned()),
                recording_path: Some(path.clone()),
                recording_append: true,
                ..base.clone()
            })
            .is_err(),
            "append accepted {bad}"
        );
        assert_eq!(std::fs::read(&path).expect("unchanged file"), bytes);
    }
    std::fs::remove_file(path).expect("remove test file");
}
