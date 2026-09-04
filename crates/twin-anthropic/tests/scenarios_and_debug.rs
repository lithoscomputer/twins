mod common;
use common::{config, request, scenario, spawn, text, TempFile};
use serde_json::{json, Value};
use twin_anthropic::config::{Config, Mode, RecordFormat};

#[tokio::test]
async fn one_shot_repeat_sticky_and_reset() {
    let server = spawn(config()).await;
    let mut repeated = scenario(json!({"kind":"success","response_text":"repeat"}));
    repeated["repeat"] = json!(2);
    let mut sticky = scenario(json!({"kind":"success","response_text":"sticky"}));
    sticky["sticky"] = json!(true);
    server
        .enqueue(
            "a",
            json!([
                scenario(json!({"kind":"success","response_text":"once"})),
                repeated,
                sticky
            ]),
        )
        .await;
    for expected in ["once", "repeat", "repeat", "sticky", "sticky"] {
        assert_eq!(text(&server.message("a", &request(false)).await), expected);
    }
    assert_eq!(
        text(&server.message("b", &request(false)).await),
        "deterministic: hello"
    );
    assert_eq!(
        server
            .post("/__admin/reset", "a", &json!({}))
            .await
            .status(),
        200
    );
    let reset = server.message("a", &request(false)).await;
    assert_eq!(reset["id"], "msg_000001");
    assert_eq!(text(&reset), "deterministic: hello");
    assert_eq!(
        server.logs("b").await["requests"]
            .as_array()
            .expect("logs")
            .len(),
        1
    );
}

#[tokio::test]
async fn startup_templates_are_strict_scoped_and_restored() {
    let file = TempFile::new();
    file.write(&json!({"scenarios":[{"scenario_id":"a-first","namespace":"a","matcher":{"endpoint":"messages"},"script":{"kind":"success","response_text":"fixture"}}]}));
    let server = spawn(Config {
        scenarios_path: Some(file.0.clone()),
        ..config()
    })
    .await;
    assert_eq!(text(&server.message("a", &request(false)).await), "fixture");
    for key in ["a", "b"] {
        let miss = server.post("/v1/messages", key, &request(false)).await;
        assert_eq!(miss.status(), 400);
        assert_eq!(
            miss.json::<Value>().await.expect("error")["error"]["code"],
            "scenario_not_found"
        );
    }
    server.post("/__admin/reset", "a", &json!({})).await;
    assert_eq!(server.logs("a").await["requests"], json!([]));
    assert_eq!(
        server.message("a", &request(false)).await["id"],
        "msg_000001"
    );
    let fallback = spawn(Config {
        scenarios_path: Some(file.0.clone()),
        allow_unmatched: true,
        ..config()
    })
    .await;
    assert_eq!(
        text(&fallback.message("b", &request(false)).await),
        "deterministic: hello"
    );
}

#[tokio::test]
async fn matchers_see_system_metadata_and_tool_results() {
    let server = spawn(config()).await;
    server.enqueue("a",json!([{"scenario_id":"matched","matcher":{"endpoint":"messages","model":"claude-test","stream":false,"metadata":{"user_id":"suite"},"input_contains":"tool result","instructions_contains":"Be terse"},"script":{"kind":"success","response_text":"matched"}}])).await;
    let req = json!({"model":"claude-test","max_tokens":128,"system":[{"type":"text","text":"Be terse","cache_control":{"type":"ephemeral"}}],"metadata":{"user_id":"suite"},"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"tool result"}]}]});
    assert_eq!(text(&server.message("a", &req).await), "matched");
    let logs = server.logs("a").await;
    assert_eq!(logs["requests"][0]["scenario_id"], "matched");
    assert_eq!(logs["requests"][0]["instructions_text"], "Be terse");
    assert_eq!(logs["requests"][0]["input_text"], "tool result");
}

#[tokio::test]
async fn scenario_validation_is_atomic() {
    let server = spawn(config()).await;
    for bad in [
        json!({"scenario_id":"","matcher":{"endpoint":"messages"},"script":{"kind":"success"}}),
        json!({"repeat":0,"matcher":{"endpoint":"messages"},"script":{"kind":"success"}}),
        json!({"matcher":{"endpoint":"typo"},"script":{"kind":"success"}}),
        scenario(
            json!({"kind":"error","status":429,"error_type":"rate_limit_error","message":"retry","retry_after":"bad\nheader"}),
        ),
        scenario(json!({"kind":"transcript","status":200,"body":{},"events":[]})),
    ] {
        let r = server
            .post("/__admin/scenarios", "a", &json!({"scenarios":[bad]}))
            .await;
        assert_eq!(r.status(), 400);
    }
    let duplicate = json!({"scenario_id":"duplicate","matcher":{"endpoint":"messages"},"script":{"kind":"success"}});
    assert_eq!(
        server
            .post(
                "/__admin/scenarios",
                "a",
                &json!({"scenarios":[duplicate.clone(),duplicate]})
            )
            .await
            .status(),
        400
    );
    assert_eq!(
        text(&server.message("a", &request(false)).await),
        "deterministic: hello"
    );
}

#[tokio::test]
async fn request_jsonl_matches_admin_and_never_contains_credentials() {
    let file = TempFile::new();
    std::fs::write(&file.0, "old records").expect("seed");
    let server = spawn(Config {
        request_log_path: Some(file.0.clone()),
        ..config()
    })
    .await;
    assert_eq!(std::fs::read_to_string(&file.0).expect("log"), "");
    server
        .message("do-not-log-this-api-key", &request(false))
        .await;
    let bytes = std::fs::read_to_string(&file.0).expect("flushed log");
    assert!(!bytes.contains("do-not-log-this-api-key"));
    assert_eq!(
        serde_json::from_str::<Value>(bytes.trim()).expect("record"),
        server.logs("do-not-log-this-api-key").await["requests"][0]
    );
}

#[tokio::test]
async fn debug_ui_exposes_state_and_escapes_markup() {
    let server = spawn(config()).await;
    let mut req = request(false);
    req["messages"][0]["content"] = json!("<script>alert('x')</script>");
    server.message("a", &req).await;
    server
        .enqueue("a", json!([scenario(json!({"kind":"success"}))]))
        .await;
    let html = server
        .get("/__debug", "a")
        .await
        .text()
        .await
        .expect("HTML");
    assert!(html.contains("twin-anthropic"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>alert"));
    let state: Value = server
        .get("/__debug/state.json", "a")
        .await
        .json()
        .await
        .expect("state");
    assert_eq!(
        state["namespaces"][0]["scenarios"][0]["script_kind"],
        "success"
    );
    assert_eq!(
        state["namespaces"][0]["request_logs"][0]["endpoint"],
        "messages"
    );
    let disabled = spawn(Config {
        enable_admin: false,
        ..config()
    })
    .await;
    for path in ["/__debug", "/__debug/state.json", "/__admin/requests"] {
        assert_eq!(disabled.get(path, "a").await.status(), 404);
    }
}

#[tokio::test]
async fn optional_auth_and_admin_bearer_alias_select_the_same_namespace() {
    let server = spawn(Config {
        require_auth: false,
        ..config()
    })
    .await;
    let response = server
        .client
        .post(format!("{}/v1/messages", server.url))
        .header("anthropic-version", "2023-06-01")
        .json(&request(false))
        .send()
        .await
        .expect("global");
    assert_eq!(response.status(), 200);
    let enqueue = server
        .client
        .post(format!("{}/__admin/scenarios", server.url))
        .bearer_auth("a")
        .json(&json!({"scenarios":[scenario(json!({"kind":"success","response_text":"aliased"}))]}))
        .send()
        .await
        .expect("enqueue");
    assert_eq!(enqueue.status(), 200);
    assert_eq!(text(&server.message("a", &request(false)).await), "aliased");
    for authorization in ["Basic a", "Bearer "] {
        assert_eq!(
            server
                .client
                .get(format!("{}/__admin/requests", server.url))
                .header("authorization", authorization)
                .send()
                .await
                .expect("get")
                .status(),
            401
        );
    }
    assert_eq!(
        server
            .client
            .get(format!("{}/__admin/requests", server.url))
            .header("x-api-key", "a")
            .bearer_auth("b")
            .send()
            .await
            .expect("get")
            .status(),
        401
    );
}

#[test]
fn configuration_and_startup_errors_are_explicit() {
    let cfg = Config::from_lookup(&|name| match name {
        "TWIN_ANTHROPIC_MODE" => Some("proxy-record".to_owned()),
        "TWIN_ANTHROPIC_RECORD_FORMAT" => Some("transcript".to_owned()),
        "TWIN_ANTHROPIC_RECORDING_APPEND" => Some("true".to_owned()),
        "TWIN_ANTHROPIC_UPSTREAM_API_KEY" => Some(String::new()),
        "ANTHROPIC_API_KEY" => Some("upstream-key".to_owned()),
        "TWIN_ANTHROPIC_RECORDING_PATH" => Some("recording.json".to_owned()),
        _ => None,
    })
    .expect("config");
    assert_eq!(cfg.mode, Mode::ProxyRecord);
    assert_eq!(cfg.record_format, RecordFormat::Transcript);
    assert!(cfg.recording_append);
    assert_eq!(cfg.upstream_api_key.as_deref(), Some("upstream-key"));
    assert_eq!(config().bind_addr.port(), 3001);
    for (name, value) in [
        ("TWIN_ANTHROPIC_REQUIRE_AUTH", "yes"),
        ("TWIN_ANTHROPIC_MODE", "bad"),
        ("TWIN_ANTHROPIC_RECORD_FORMAT", "bad"),
        ("TWIN_ANTHROPIC_BIND_ADDR", "bad"),
        ("TWIN_ANTHROPIC_UPSTREAM_MESSAGES_PATH", "no-slash"),
    ] {
        assert!(Config::from_lookup(&|key| (key == name).then(|| value.to_owned())).is_err());
    }
    assert!(twin_anthropic::build_app_with_config(Config {
        mode: Mode::ProxyRecord,
        ..config()
    })
    .is_err());
    let file = TempFile::new();
    file.write(&json!({"wrong":[]}));
    assert!(twin_anthropic::build_app_with_config(Config {
        scenarios_path: Some(file.0.clone()),
        ..config()
    })
    .is_err());
    assert!(twin_anthropic::build_app_with_config(Config {
        request_log_path: Some(std::env::temp_dir()),
        ..config()
    })
    .is_err());
}
