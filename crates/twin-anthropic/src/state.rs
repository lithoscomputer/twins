use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::config::Config;
use crate::engine::scenario::{validate_scenario_ids, RequestContext, Scenario, ScenarioEnvelope};
use crate::logs::RequestLog;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum NamespaceKey {
    Global,
    ApiKey(String),
}

impl fmt::Display for NamespaceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => write!(f, "Global"),
            Self::ApiKey(token) => write!(f, "ApiKey: {token}"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DebugSnapshot {
    pub namespaces: Vec<NamespaceSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NamespaceSnapshot {
    pub key: String,
    pub scenarios: Vec<ScenarioSnapshot>,
    pub request_logs: Vec<RequestLog>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScenarioSnapshot {
    pub scenario_id: Option<String>,
    pub endpoint: String,
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub input_contains: Option<String>,
    pub instructions_contains: Option<String>,
    pub metadata: serde_json::Map<String, Value>,
    pub script_kind: String,
    /// Answers left before the scenario is spent, or `None` when sticky.
    pub remaining: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub config: Config,
    inner: Arc<AppStateInner>,
}

#[derive(Debug)]
struct AppStateInner {
    namespaces: Mutex<HashMap<NamespaceKey, NamespaceState>>,
    request_log_writer: Option<Mutex<JsonlRequestLogWriter>>,
    scenario_template: Vec<Scenario>,
}

#[derive(Debug)]
struct JsonlRequestLogWriter {
    writer: BufWriter<File>,
}

#[derive(Debug)]
struct NamespaceState {
    next_response_number: u64,
    scenarios: Vec<Scenario>,
    request_logs: Vec<RequestLog>,
}

impl Default for NamespaceState {
    fn default() -> Self {
        Self {
            next_response_number: 1,
            scenarios: Vec::new(),
            request_logs: Vec::new(),
        }
    }
}

impl NamespaceState {
    fn with_scenarios(scenarios: Vec<Scenario>) -> Self {
        Self {
            scenarios,
            ..Self::default()
        }
    }
}

impl AppState {
    pub fn new(config: Config) -> Result<Self> {
        let request_log_writer = config
            .request_log_path
            .as_deref()
            .map(JsonlRequestLogWriter::open)
            .transpose()?
            .map(Mutex::new);
        let scenario_template = config
            .scenarios_path
            .as_deref()
            .map(load_scenario_template)
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            config,
            inner: Arc::new(AppStateInner {
                namespaces: Mutex::new(HashMap::new()),
                request_log_writer,
                scenario_template,
            }),
        })
    }

    pub fn next_response_id(&self, namespace: &NamespaceKey) -> u64 {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let namespace_state = self.namespace_state(&mut namespaces, namespace);
        let response_id = namespace_state.next_response_number;
        namespace_state.next_response_number += 1;
        response_id
    }

    pub fn enqueue_scenarios(
        &self,
        namespace: &NamespaceKey,
        mut scenarios: Vec<Scenario>,
    ) -> Result<(), String> {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let namespace_state = self.namespace_state(&mut namespaces, namespace);
        validate_scenario_ids(namespace_state.scenarios.iter().chain(scenarios.iter()))?;
        namespace_state.scenarios.append(&mut scenarios);
        Ok(())
    }

    /// The first scenario in queue order that matches `request`.
    ///
    /// A one-shot scenario is removed. A `repeat` scenario answers again
    /// until its count is spent. A `sticky` scenario stays until the
    /// namespace is reset.
    pub fn take_matching_scenario(
        &self,
        namespace: &NamespaceKey,
        request: &RequestContext,
    ) -> Option<Scenario> {
        self.take_matching_scenario_if(namespace, request, |_| true)
    }

    /// Check the first matching scenario before consuming it. A rejected
    /// candidate stays in place; later entries never jump ahead of it.
    pub(crate) fn take_matching_scenario_if(
        &self,
        namespace: &NamespaceKey,
        request: &RequestContext,
        accept: impl FnOnce(&Scenario) -> bool,
    ) -> Option<Scenario> {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let scenarios = &mut self.namespace_state(&mut namespaces, namespace).scenarios;
        let position = scenarios
            .iter()
            .position(|scenario| scenario.matches(request))?;
        let scenario = &mut scenarios[position];
        if !accept(scenario) {
            return None;
        }
        if scenario.sticky {
            return Some(scenario.clone());
        }
        if scenario.repeat > 1 {
            scenario.repeat -= 1;
            return Some(scenario.clone());
        }
        Some(scenarios.remove(position))
    }

    pub fn log_request(
        &self,
        namespace: &NamespaceKey,
        request: RequestContext,
        scenario_id: Option<String>,
    ) {
        let request_log = RequestLog {
            scenario_id,
            endpoint: request.endpoint,
            model: request.model,
            stream: request.stream,
            input_text: request.input_text,
            instructions_text: request.instructions_text,
            metadata: request.metadata,
        };
        let mut request_log_writer = self.inner.request_log_writer.as_ref().map(|writer| {
            writer
                .lock()
                .expect("request JSONL writer lock should not be poisoned")
        });

        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        self.namespace_state(&mut namespaces, namespace)
            .request_logs
            .push(request_log.clone());

        if let Some(writer) = request_log_writer.as_mut() {
            if let Err(error) = writer.write_record(&request_log) {
                tracing::error!(%error, "failed to append twin-anthropic request JSONL record");
            }
        }
    }

    pub fn request_logs(&self, namespace: &NamespaceKey) -> Vec<RequestLog> {
        self.inner
            .namespaces
            .lock()
            .expect("namespaces lock")
            .get(namespace)
            .map(|namespace_state| namespace_state.request_logs.clone())
            .unwrap_or_default()
    }

    pub fn reset(&self, namespace: &NamespaceKey) {
        self.inner
            .namespaces
            .lock()
            .expect("namespaces lock")
            .insert(
                namespace.clone(),
                NamespaceState::with_scenarios(self.template_for(namespace)),
            );
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        let namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let mut result = Vec::new();
        for (key, ns) in namespaces.iter() {
            result.push(NamespaceSnapshot {
                key: key.to_string(),
                scenarios: ns
                    .scenarios
                    .iter()
                    .map(|s| ScenarioSnapshot {
                        scenario_id: s.scenario_id.clone(),
                        endpoint: s.matcher.endpoint.clone(),
                        model: s.matcher.model.clone(),
                        stream: s.matcher.stream,
                        input_contains: s.matcher.input_contains.clone(),
                        instructions_contains: s.matcher.instructions_contains.clone(),
                        metadata: s.matcher.metadata.clone(),
                        script_kind: s.script.script_kind().to_owned(),
                        remaining: (!s.sticky).then_some(s.repeat),
                    })
                    .collect(),
                request_logs: ns.request_logs.clone(),
            });
        }
        DebugSnapshot { namespaces: result }
    }

    fn namespace_state<'a>(
        &self,
        namespaces: &'a mut HashMap<NamespaceKey, NamespaceState>,
        namespace: &NamespaceKey,
    ) -> &'a mut NamespaceState {
        namespaces
            .entry(namespace.clone())
            .or_insert_with(|| NamespaceState::with_scenarios(self.template_for(namespace)))
    }

    /// Startup-template scenarios seeded into a namespace: scenarios without
    /// a `namespace` seed everywhere, namespaced scenarios seed only their
    /// own API-key namespace.
    fn template_for(&self, namespace: &NamespaceKey) -> Vec<Scenario> {
        self.inner
            .scenario_template
            .iter()
            .filter(|scenario| match &scenario.namespace {
                None => true,
                Some(token) => {
                    matches!(namespace, NamespaceKey::ApiKey(key) if key == token)
                }
            })
            .cloned()
            .collect()
    }
}

fn load_scenario_template(path: &Path) -> Result<Vec<Scenario>> {
    let contents = fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read twin-anthropic scenarios from {}",
            path.display()
        )
    })?;
    let envelope: ScenarioEnvelope = serde_json::from_str(&contents).with_context(|| {
        format!(
            "failed to parse twin-anthropic scenarios from {}",
            path.display()
        )
    })?;
    validate_scenario_ids(&envelope.scenarios)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("invalid twin-anthropic scenarios in {}", path.display()))?;
    Ok(envelope.scenarios)
}

impl JsonlRequestLogWriter {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create twin-anthropic request log directory {}",
                    parent.display()
                )
            })?;
        }

        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| {
                format!(
                    "failed to create twin-anthropic request log {}",
                    path.display()
                )
            })?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn write_record(&mut self, request: &RequestLog) -> Result<()> {
        serde_json::to_writer(&mut self.writer, request)
            .context("failed to serialize twin-anthropic request log record")?;
        self.writer
            .write_all(b"\n")
            .context("failed to terminate twin-anthropic request log record")?;
        self.writer
            .flush()
            .context("failed to flush twin-anthropic request log record")
    }
}
