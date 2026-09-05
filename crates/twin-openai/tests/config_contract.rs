use twin_openai::config::Config;

#[test]
fn config_loads_from_environment() {
    let config = Config::from_lookup(&|name| match name {
        "TWIN_OPENAI_BIND_ADDR" => Some("127.0.0.1:4100".to_string()),
        "TWIN_OPENAI_REQUIRE_AUTH" | "TWIN_OPENAI_ENABLE_ADMIN" => Some("false".to_string()),
        "TWIN_OPENAI_REQUEST_LOG_PATH" => Some("tmp/requests.jsonl".to_string()),
        "TWIN_OPENAI_SCENARIOS_PATH" => Some("fixtures/scenarios.json".to_string()),
        "TWIN_OPENAI_ALLOW_UNMATCHED" => Some("true".to_string()),
        "TWIN_OPENAI_UPSTREAM_URL" => Some("https://gateway.example/openai/".to_owned()),
        "TWIN_OPENAI_UPSTREAM_RESPONSES_PATH" => Some("/responses".to_owned()),
        _ => None,
    })
    .expect("config should load");

    assert_eq!(config.bind_addr.to_string(), "127.0.0.1:4100");
    assert!(!config.require_auth);
    assert!(!config.enable_admin);
    assert_eq!(
        config.request_log_path.as_deref(),
        Some(std::path::Path::new("tmp/requests.jsonl"))
    );
    assert_eq!(
        config.scenarios_path.as_deref(),
        Some(std::path::Path::new("fixtures/scenarios.json"))
    );
    assert!(config.allow_unmatched);
    assert_eq!(config.upstream_url, "https://gateway.example/openai");
    assert_eq!(
        config.upstream_responses_path.as_deref(),
        Some("/responses")
    );
}

#[test]
fn absent_environment_preserves_openai_defaults() {
    let config = Config::from_lookup(&|_| None).expect("defaults");
    assert_eq!(config.bind_addr.to_string(), "127.0.0.1:3000");
    assert_eq!(config.upstream_url, "https://api.openai.com");
    assert!(config.upstream_responses_path.is_none());
}

#[test]
fn empty_upstream_override_uses_the_standard_api_key() {
    let config = Config::from_lookup(&|name| match name {
        "TWIN_OPENAI_MODE" => Some("proxy-record".to_owned()),
        "TWIN_OPENAI_RECORDING_PATH" => Some("recording.json".to_owned()),
        "TWIN_OPENAI_UPSTREAM_API_KEY" => Some(String::new()),
        "OPENAI_API_KEY" => Some("standard-key".to_owned()),
        _ => None,
    })
    .expect("empty override should fall back to OPENAI_API_KEY");
    assert_eq!(config.upstream_api_key.as_deref(), Some("standard-key"));
}

#[test]
fn proxy_record_rejects_blank_keys_in_programmatic_configs() {
    let base = Config::from_lookup(&|_| None).expect("default configuration");
    for key in [None, Some(""), Some(" \t\n")] {
        let config = Config {
            mode: twin_openai::config::Mode::ProxyRecord,
            upstream_api_key: key.map(str::to_owned),
            recording_path: Some("recording.json".into()),
            ..base.clone()
        };
        assert!(config.validate().is_err());
    }
}
