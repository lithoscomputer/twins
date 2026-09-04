//! Opt-in live smoke/capture suite. Normal workspace tests never call Anthropic.
mod common;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, fs, path::Path};
use twin_anthropic::record::{derive_script, message_from_events, parse_sse_events};

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY and explicit live API access"]
async fn live_anthropic_contract() -> Result<()> {
    let LiveConfig {
        key,
        base,
        model,
        record,
    } = LiveConfig::from_env(&ProcessEnvironment)?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut fixture: Value = serde_json::from_slice(&fs::read(root.join("contracts.json"))?)?;
    let case_count = fixture["cases"].as_array().context("cases")?.len();
    anyhow::ensure!(case_count > 0, "no live Messages cases configured");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_mins(3))
        .build()?;
    let mut scenarios = Vec::new();
    let mut differences = Vec::new();
    for case in fixture["cases"].as_array_mut().context("cases")? {
        case["request"]["model"] = json!(model);
        let response = client
            .post(format!("{}/v1/messages", base.trim_end_matches('/')))
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .json(&case["request"])
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        // Do not replace a complete fixture set with a partial capture.
        anyhow::ensure!(
            status.is_success(),
            "case {} returned HTTP {status}: {}",
            case["case_id"],
            String::from_utf8_lossy(&bytes)
        );
        let (message, events) = if case["request"]["stream"] == true {
            let events = parse_sse_events(&bytes)?;
            (message_from_events(&events)?, Some(events))
        } else {
            (serde_json::from_slice(&bytes)?, None)
        };
        let canonical = common::contracts::canonical(&message, events.as_deref());
        let extras = common::contracts::extras(&message);
        if canonical != case["canonical"] || extras != case["extra_fields"] {
            differences.push(case["case_id"].to_string());
        }
        if record {
            case["canonical"] = canonical;
            case["extra_fields"] = extras;
        }
        let id = case["case_id"].as_str().context("case id")?;
        scenarios.push(json!({"scenario_id":id,"matcher":{"endpoint":"messages","stream":case["request"]["stream"],"input_contains":format!("[case:{id}]")},"script":derive_script(&message)?}));
    }
    let count = client
        .post(format!(
            "{}/v1/messages/count_tokens",
            base.trim_end_matches('/')
        ))
        .header("x-api-key", &key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({"model":model,"messages":[{"role":"user","content":"Hello"}]}))
        .send()
        .await?;
    anyhow::ensure!(
        count.status().is_success(),
        "count_tokens returned HTTP {}",
        count.status()
    );
    anyhow::ensure!(
        count.json::<Value>().await?["input_tokens"].is_u64(),
        "invalid token count"
    );
    let models = client
        .get(format!("{}/v1/models", base.trim_end_matches('/')))
        .header("x-api-key", &key)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await?;
    anyhow::ensure!(
        models.status().is_success(),
        "models returned HTTP {}",
        models.status()
    );
    anyhow::ensure!(
        models.json::<Value>().await?["data"].is_array(),
        "invalid model list"
    );
    eprintln!("live contract: completed {case_count} Messages cases, count_tokens, and models");
    if record {
        fixture["provenance"] = json!("live Anthropic API capture");
        write_atomic(
            &root.join("scenarios.json"),
            &json!({"scenarios":scenarios}),
        )?;
        write_atomic(&root.join("contracts.json"), &fixture)?;
    } else {
        anyhow::ensure!(
            differences.is_empty(),
            "live contract drift: {}. Run mise run record:anthropic to capture changes.",
            differences.join(", ")
        );
    }
    Ok(())
}

trait Environment {
    fn get(&self, name: &str) -> Option<String>;
}

struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

struct LiveConfig {
    key: String,
    base: String,
    model: String,
    record: bool,
}

impl LiveConfig {
    fn from_env(environment: &impl Environment) -> Result<Self> {
        let key = environment
            .get("ANTHROPIC_API_KEY")
            .filter(|key| !key.trim().is_empty())
            .context("ANTHROPIC_API_KEY must be set and non-empty for an explicit live run")?;
        Ok(Self {
            key,
            base: environment
                .get("TWIN_ANTHROPIC_LIVE_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".to_owned()),
            model: environment
                .get("TWIN_ANTHROPIC_LIVE_MODEL")
                .unwrap_or_else(|| "claude-sonnet-4-6".to_owned()),
            record: environment
                .get("TWIN_ANTHROPIC_RECORD_FIXTURES")
                .is_some_and(|v| v == "1"),
        })
    }
}

struct FakeEnvironment(HashMap<&'static str, String>);

impl Environment for FakeEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

#[test]
fn explicit_live_runs_require_nonempty_credentials() {
    for value in [None, Some(String::new()), Some(" \t\n".to_owned())] {
        let environment = FakeEnvironment(
            value
                .map(|key| ("ANTHROPIC_API_KEY", key))
                .into_iter()
                .collect(),
        );
        assert!(LiveConfig::from_env(&environment).is_err());
    }
    let environment = FakeEnvironment(HashMap::from([(
        "ANTHROPIC_API_KEY",
        "test-key".to_owned(),
    )]));
    let config = LiveConfig::from_env(&environment).expect("configured key");
    assert_eq!(config.key, "test-key");
    assert_eq!(config.base, "https://api.anthropic.com");
    assert_eq!(config.model, "claude-sonnet-4-6");
    assert!(!config.record);
}

#[test]
fn live_configuration_reads_overrides_from_a_fake_environment() {
    let environment = FakeEnvironment(HashMap::from([
        ("ANTHROPIC_API_KEY", "test-key".to_owned()),
        (
            "TWIN_ANTHROPIC_LIVE_BASE_URL",
            "http://example.invalid".to_owned(),
        ),
        ("TWIN_ANTHROPIC_LIVE_MODEL", "claude-test".to_owned()),
        ("TWIN_ANTHROPIC_RECORD_FIXTURES", "1".to_owned()),
    ]));
    let config = LiveConfig::from_env(&environment).expect("fake config");
    assert_eq!(config.base, "http://example.invalid");
    assert_eq!(config.model, "claude-test");
    assert!(config.record);
}

fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)?;
    Ok(())
}
