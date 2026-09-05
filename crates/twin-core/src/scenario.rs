use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Deserialize)]
pub struct ScenarioEnvelope<S> {
    pub scenarios: Vec<S>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScenarioMatcher {
    pub endpoint: String,
    pub model: Option<String>,
    pub stream: Option<bool>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    pub input_contains: Option<String>,
    /// Substring of the request's instructions: the `instructions` field
    /// plus any `system` or `developer` messages. Lets a test prove that a
    /// system prompt reached the model.
    pub instructions_contains: Option<String>,
    /// Hash of the canonicalized request body, as proxy-record writes it in
    /// transcript format. A hashed scenario matches only the exact request
    /// it was recorded from, so replay does not depend on request order.
    pub request_hash: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub endpoint: String,
    pub model: String,
    pub stream: bool,
    pub metadata: Map<String, Value>,
    pub input_text: String,
    pub instructions_text: String,
    /// Hash of the canonicalized request body, for transcript matching.
    pub request_hash: Option<String>,
}

impl ScenarioMatcher {
    pub fn matches(&self, request: &RequestContext) -> bool {
        self.endpoint == request.endpoint
            && self.model.as_ref().is_none_or(|m| m == &request.model)
            && self.stream.is_none_or(|s| s == request.stream)
            && self
                .input_contains
                .as_ref()
                .is_none_or(|s| request.input_text.contains(s))
            && self
                .instructions_contains
                .as_ref()
                .is_none_or(|s| request.instructions_text.contains(s))
            && self
                .request_hash
                .as_ref()
                .is_none_or(|s| request.request_hash.as_ref() == Some(s))
            && self
                .metadata
                .iter()
                .all(|(k, v)| request.metadata.get(k) == Some(v))
    }
}

/// The fields needed to store scenarios. Scripts and their validation stay
/// in the provider crate; the store never interprets a response script.
pub trait QueuedScenario: Clone + DeserializeOwned {
    fn scenario_id(&self) -> Option<&str>;
    fn namespace(&self) -> Option<&str>;
    fn matcher(&self) -> &ScenarioMatcher;
    fn repeat(&self) -> u32;
    fn repeat_mut(&mut self) -> &mut u32;
    fn sticky(&self) -> bool;
    fn script_kind(&self) -> &str;
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

pub fn validate_scenarios<'a, S: QueuedScenario + 'a>(
    scenarios: impl IntoIterator<Item = &'a S>,
) -> Result<(), String> {
    let mut ids = HashSet::new();
    for scenario in scenarios {
        if let Some(id) = scenario.scenario_id() {
            if id.trim().is_empty() {
                return Err("scenario_id must not be empty".to_owned());
            }
            if !ids.insert(id) {
                return Err(format!("duplicate scenario_id: {id}"));
            }
        }
        scenario.validate()?;
    }
    Ok(())
}

pub fn parse_scenarios<S: QueuedScenario>(contents: &[u8]) -> Result<Vec<S>> {
    let envelope: ScenarioEnvelope<S> = serde_json::from_slice(contents)?;
    validate_scenarios(&envelope.scenarios).map_err(anyhow::Error::msg)?;
    Ok(envelope.scenarios)
}

pub fn load_scenarios<S: QueuedScenario>(path: &Path, twin_name: &str) -> Result<Vec<S>> {
    let contents = fs::read(path).with_context(|| {
        format!(
            "failed to read {twin_name} scenarios from {}",
            path.display()
        )
    })?;
    let envelope: ScenarioEnvelope<S> = serde_json::from_slice(&contents).with_context(|| {
        format!(
            "failed to parse {twin_name} scenarios from {}",
            path.display()
        )
    })?;
    validate_scenarios(&envelope.scenarios)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("invalid {twin_name} scenarios in {}", path.display()))?;
    Ok(envelope.scenarios)
}
