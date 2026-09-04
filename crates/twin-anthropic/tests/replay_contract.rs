mod common;
use serde_json::Value;
use twin_anthropic::config::Config;
use twin_anthropic::record::{message_from_events, parse_sse_events};

#[tokio::test]
async fn replay_recorded_contracts_offline() {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/contracts.json")).expect("contracts");
    let server = common::spawn(Config {
        scenarios_path: Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/scenarios.json"),
        ),
        ..common::config()
    })
    .await;
    for case in fixture["cases"].as_array().expect("cases") {
        let response = server
            .post("/v1/messages", "replay", &case["request"])
            .await;
        assert_eq!(response.status(), 200, "case {}", case["case_id"]);
        let bytes = response.bytes().await.expect("body");
        let (message, events) = if case["request"]["stream"] == true {
            let events = parse_sse_events(&bytes).expect("SSE");
            (message_from_events(&events).expect("message"), Some(events))
        } else {
            (serde_json::from_slice(&bytes).expect("JSON"), None)
        };
        assert_eq!(
            common::contracts::canonical(&message, events.as_deref()),
            case["canonical"],
            "case {}",
            case["case_id"]
        );
    }
}
