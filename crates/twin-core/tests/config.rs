use twin_core::config::{Mode, ProviderConfig, RecordFormat};

const PROVIDER: ProviderConfig = ProviderConfig {
    env_prefix: "TWIN_TEST",
    api_key_env: "TEST_API_KEY",
    upstream_path_env: "TWIN_TEST_UPSTREAM_GENERATION_PATH",
    default_port: 4100,
    default_upstream_url: "https://upstream.example",
};

#[test]
fn absent_environment_uses_provider_and_shared_defaults() {
    let settings = PROVIDER.load(&|_| None).expect("defaults");
    assert_eq!(settings, PROVIDER.defaults());
    assert_eq!(settings.bind_addr.to_string(), "127.0.0.1:4100");
    assert_eq!(settings.upstream_url, "https://upstream.example");
    assert_eq!(settings.mode, Mode::Twin);
    assert_eq!(settings.record_format, RecordFormat::Semantic);
    assert!(settings.require_auth && settings.enable_admin);
    assert!(!settings.allow_unmatched && !settings.recording_append);
    assert!(settings.request_log_path.is_none());
    assert!(settings.scenarios_path.is_none());
    assert!(settings.recording_path.is_none());
    assert!(settings.upstream_path.is_none());
    assert!(settings.upstream_api_key.is_none());
}

#[test]
fn environment_loads_every_setting() {
    let settings = PROVIDER
        .load(&|name| {
            Some(
                match name {
                    "TWIN_TEST_BIND_ADDR" => "0.0.0.0:4200",
                    "TWIN_TEST_REQUIRE_AUTH" => "0",
                    "TWIN_TEST_ENABLE_ADMIN" => "false",
                    "TWIN_TEST_REQUEST_LOG_PATH" => "logs/requests.jsonl",
                    "TWIN_TEST_SCENARIOS_PATH" => "fixtures/scenarios.json",
                    "TWIN_TEST_ALLOW_UNMATCHED" => "1",
                    "TWIN_TEST_MODE" => "proxy-record",
                    "TWIN_TEST_UPSTREAM_URL" => "https://gateway.example/base///",
                    "TWIN_TEST_UPSTREAM_GENERATION_PATH" => "/custom/generate",
                    "TWIN_TEST_UPSTREAM_API_KEY" => "override-key",
                    "TEST_API_KEY" => "fallback-key",
                    "TWIN_TEST_RECORDING_PATH" => "recordings/capture.json",
                    "TWIN_TEST_RECORD_FORMAT" => "transcript",
                    "TWIN_TEST_RECORDING_APPEND" => "true",
                    _ => return None,
                }
                .to_owned(),
            )
        })
        .expect("configured settings");
    assert_eq!(settings.bind_addr.to_string(), "0.0.0.0:4200");
    assert!(!settings.require_auth && !settings.enable_admin);
    assert!(settings.allow_unmatched && settings.recording_append);
    assert_eq!(
        settings.request_log_path,
        Some("logs/requests.jsonl".into())
    );
    assert_eq!(
        settings.scenarios_path,
        Some("fixtures/scenarios.json".into())
    );
    assert_eq!(settings.mode, Mode::ProxyRecord);
    assert_eq!(settings.upstream_url, "https://gateway.example/base");
    assert_eq!(settings.upstream_path.as_deref(), Some("/custom/generate"));
    assert_eq!(settings.upstream_api_key.as_deref(), Some("override-key"));
    assert_eq!(
        settings.recording_path,
        Some("recordings/capture.json".into())
    );
    assert_eq!(settings.record_format, RecordFormat::Transcript);
}

#[test]
fn empty_optional_values_keep_defaults_and_allow_the_fallback_key() {
    let settings = PROVIDER
        .load(&|name| match name {
            "TWIN_TEST_REQUEST_LOG_PATH"
            | "TWIN_TEST_SCENARIOS_PATH"
            | "TWIN_TEST_UPSTREAM_URL"
            | "TWIN_TEST_UPSTREAM_GENERATION_PATH"
            | "TWIN_TEST_UPSTREAM_API_KEY"
            | "TWIN_TEST_RECORDING_PATH" => Some(String::new()),
            "TEST_API_KEY" => Some("fallback-key".to_owned()),
            _ => None,
        })
        .expect("empty optional values");
    let mut expected = PROVIDER.defaults();
    expected.upstream_api_key = Some("fallback-key".to_owned());
    assert_eq!(settings, expected);
}

#[test]
fn invalid_values_name_the_provider_environment_variable() {
    for (name, value) in [
        ("TWIN_TEST_BIND_ADDR", "invalid"),
        ("TWIN_TEST_REQUIRE_AUTH", "yes"),
        ("TWIN_TEST_ENABLE_ADMIN", "yes"),
        ("TWIN_TEST_ALLOW_UNMATCHED", "yes"),
        ("TWIN_TEST_RECORDING_APPEND", "yes"),
        ("TWIN_TEST_MODE", ""),
        ("TWIN_TEST_RECORD_FORMAT", "raw"),
        ("TWIN_TEST_UPSTREAM_GENERATION_PATH", "no-leading-slash"),
    ] {
        let error = PROVIDER
            .load(&|key| (key == name).then(|| value.to_owned()))
            .expect_err("invalid setting");
        assert!(error.to_string().contains(name), "{error}");
    }
}

#[test]
fn proxy_record_requires_a_key_and_recording_path() {
    for key in [None, Some(""), Some(" \t")] {
        let error = PROVIDER
            .load(&|name| match name {
                "TWIN_TEST_MODE" => Some("proxy-record".to_owned()),
                "TWIN_TEST_RECORDING_PATH" => Some("recording.json".to_owned()),
                "TEST_API_KEY" => key.map(str::to_owned),
                _ => None,
            })
            .expect_err("missing or blank key");
        assert!(error
            .to_string()
            .contains("TWIN_TEST_UPSTREAM_API_KEY or TEST_API_KEY"));
    }
    let error = PROVIDER
        .load(&|name| match name {
            "TWIN_TEST_MODE" => Some("proxy-record".to_owned()),
            "TEST_API_KEY" => Some("key".to_owned()),
            _ => None,
        })
        .expect_err("missing recording path");
    assert!(error.to_string().contains("TWIN_TEST_RECORDING_PATH"));
}
