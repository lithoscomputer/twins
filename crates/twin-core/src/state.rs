use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::logs::{JsonlRequestLogWriter, RequestLog};
use crate::scenario::{validate_scenarios, QueuedScenario, RequestContext};

/// Providers decide how authentication maps to a namespace and how it is shown.
pub trait Namespace: Clone + Eq + Hash + fmt::Display {
    fn credential(&self) -> Option<&str>;
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
pub struct ScenarioStore<S, N> {
    inner: Arc<StoreInner<S, N>>,
}

#[derive(Debug)]
struct StoreInner<S, N> {
    namespaces: Mutex<HashMap<N, NamespaceState<S>>>,
    request_log_writer: Option<Mutex<JsonlRequestLogWriter>>,
    scenario_template: Vec<S>,
}

#[derive(Debug)]
struct NamespaceState<S> {
    next_response_number: u64,
    scenarios: Vec<S>,
    request_logs: Vec<RequestLog>,
}

impl<S> Default for NamespaceState<S> {
    fn default() -> Self {
        Self {
            next_response_number: 1,
            scenarios: Vec::new(),
            request_logs: Vec::new(),
        }
    }
}

impl<S> NamespaceState<S> {
    fn with_scenarios(scenarios: Vec<S>) -> Self {
        Self {
            scenarios,
            ..Self::default()
        }
    }
}

impl<S: QueuedScenario, N: Namespace> ScenarioStore<S, N> {
    pub fn new(
        scenario_template: Vec<S>,
        request_log_path: Option<&Path>,
        twin_name: &'static str,
    ) -> Result<Self> {
        validate_scenarios(&scenario_template).map_err(anyhow::Error::msg)?;
        let request_log_writer = request_log_path
            .map(|path| JsonlRequestLogWriter::open(path, twin_name))
            .transpose()?
            .map(Mutex::new);
        Ok(Self {
            inner: Arc::new(StoreInner {
                namespaces: Mutex::new(HashMap::new()),
                request_log_writer,
                scenario_template,
            }),
        })
    }

    pub fn next_response_id(&self, namespace: &N) -> u64 {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let namespace_state = self.namespace_state(&mut namespaces, namespace);
        let response_id = namespace_state.next_response_number;
        namespace_state.next_response_number += 1;
        response_id
    }

    pub fn enqueue_scenarios(&self, namespace: &N, mut scenarios: Vec<S>) -> Result<(), String> {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let namespace_state = self.namespace_state(&mut namespaces, namespace);
        validate_scenarios(namespace_state.scenarios.iter().chain(scenarios.iter()))?;
        namespace_state.scenarios.append(&mut scenarios);
        Ok(())
    }

    /// The first scenario in queue order that matches `request`.
    ///
    /// A one-shot scenario is removed. A `repeat` scenario answers again
    /// until its count is spent. A `sticky` scenario stays until the
    /// namespace is reset.
    pub fn take_matching_scenario(&self, namespace: &N, request: &RequestContext) -> Option<S> {
        self.take_matching_scenario_if(namespace, request, |_| true)
    }

    /// Check the first matching scenario before consuming it. A rejected
    /// candidate stays in place; later entries never jump ahead of it.
    pub fn take_matching_scenario_if(
        &self,
        namespace: &N,
        request: &RequestContext,
        accept: impl FnOnce(&S) -> bool,
    ) -> Option<S> {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let scenarios = &mut self.namespace_state(&mut namespaces, namespace).scenarios;
        let position = scenarios
            .iter()
            .position(|scenario| scenario.matcher().matches(request))?;
        let scenario = &mut scenarios[position];
        if !accept(scenario) {
            return None;
        }
        if scenario.sticky() {
            return Some(scenario.clone());
        }
        if scenario.repeat() > 1 {
            *scenario.repeat_mut() -= 1;
            return Some(scenario.clone());
        }
        Some(scenarios.remove(position))
    }

    pub fn log_request(&self, namespace: &N, request: RequestContext, scenario_id: Option<String>) {
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
                tracing::error!(%error, "failed to append request JSONL record");
            }
        }
    }

    pub fn request_logs(&self, namespace: &N) -> Vec<RequestLog> {
        self.inner
            .namespaces
            .lock()
            .expect("namespaces lock")
            .get(namespace)
            .map(|namespace_state| namespace_state.request_logs.clone())
            .unwrap_or_default()
    }

    pub fn reset(&self, namespace: &N) {
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
                        scenario_id: s.scenario_id().map(str::to_owned),
                        endpoint: s.matcher().endpoint.clone(),
                        model: s.matcher().model.clone(),
                        stream: s.matcher().stream,
                        input_contains: s.matcher().input_contains.clone(),
                        instructions_contains: s.matcher().instructions_contains.clone(),
                        metadata: s.matcher().metadata.clone(),
                        script_kind: s.script_kind().to_owned(),
                        remaining: (!s.sticky()).then_some(s.repeat()),
                    })
                    .collect(),
                request_logs: ns.request_logs.clone(),
            });
        }
        DebugSnapshot { namespaces: result }
    }

    fn namespace_state<'a>(
        &self,
        namespaces: &'a mut HashMap<N, NamespaceState<S>>,
        namespace: &N,
    ) -> &'a mut NamespaceState<S> {
        namespaces
            .entry(namespace.clone())
            .or_insert_with(|| NamespaceState::with_scenarios(self.template_for(namespace)))
    }

    /// Startup-template scenarios seeded into a namespace: scenarios without
    /// a `namespace` seed everywhere, namespaced scenarios seed only their
    /// own credential namespace.
    fn template_for(&self, namespace: &N) -> Vec<S> {
        self.inner
            .scenario_template
            .iter()
            .filter(|scenario| {
                scenario
                    .namespace()
                    .is_none_or(|token| namespace.credential() == Some(token))
            })
            .cloned()
            .collect()
    }
}
