use crate::config::Config;
use crate::engine::scenario::{RequestContext, Scenario};
use crate::logs::RequestLog;
use anyhow::Result;
use std::fmt;
use twin_core::scenario::load_scenarios;
pub use twin_core::state::{DebugSnapshot, NamespaceSnapshot, ScenarioSnapshot};
use twin_core::state::{Namespace, ScenarioStore};

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

impl Namespace for NamespaceKey {
    fn credential(&self) -> Option<&str> {
        match self {
            Self::Global => None,
            Self::ApiKey(key) => Some(key),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub config: Config,
    store: ScenarioStore<Scenario, NamespaceKey>,
}

impl AppState {
    pub fn new(config: Config) -> Result<Self> {
        let scenarios = config
            .scenarios_path
            .as_deref()
            .map(|path| load_scenarios(path, "twin-anthropic"))
            .transpose()?
            .unwrap_or_default();
        let store = ScenarioStore::new(
            scenarios,
            config.request_log_path.as_deref(),
            "twin-anthropic",
        )?;
        Ok(Self { config, store })
    }

    pub fn next_response_id(&self, namespace: &NamespaceKey) -> u64 {
        self.store.next_response_id(namespace)
    }

    pub fn enqueue_scenarios(
        &self,
        namespace: &NamespaceKey,
        scenarios: Vec<Scenario>,
    ) -> Result<(), String> {
        self.store.enqueue_scenarios(namespace, scenarios)
    }

    pub fn take_matching_scenario(
        &self,
        namespace: &NamespaceKey,
        request: &RequestContext,
    ) -> Option<Scenario> {
        self.store.take_matching_scenario(namespace, request)
    }

    pub(crate) fn take_matching_scenario_if(
        &self,
        namespace: &NamespaceKey,
        request: &RequestContext,
        accept: impl FnOnce(&Scenario) -> bool,
    ) -> Option<Scenario> {
        self.store
            .take_matching_scenario_if(namespace, request, accept)
    }

    pub fn log_request(
        &self,
        namespace: &NamespaceKey,
        request: RequestContext,
        scenario_id: Option<String>,
    ) {
        self.store.log_request(namespace, request, scenario_id);
    }

    pub fn request_logs(&self, namespace: &NamespaceKey) -> Vec<RequestLog> {
        self.store.request_logs(namespace)
    }

    pub fn reset(&self, namespace: &NamespaceKey) {
        self.store.reset(namespace);
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        self.store.debug_snapshot()
    }
}
