use crate::scenario::{parse_scenarios, QueuedScenario};
use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Hash of a request body for transcript matching.
///
/// Object keys are sorted recursively before hashing, so whitespace and key
/// order do not affect matching. Array order is preserved. The hash is FNV-1a
/// over that canonical text — a matcher key, not a security boundary.
#[must_use]
pub fn request_hash(body: &[u8]) -> Option<String> {
    let mut value: Value = serde_json::from_slice(body).ok()?;
    value.sort_all_objects();
    let canonical = value.to_string();

    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(format!("{hash:016x}"))
}

pub struct RecordingStore {
    path: PathBuf,
    state: Mutex<RecorderState>,
}

#[derive(Default)]
struct RecorderState {
    scenarios: Vec<Value>,
    counters: HashMap<String, u64>,
}

impl RecordingStore {
    /// Creates the recorder. By default the recording file is truncated, so
    /// a server run produces a complete, self-consistent recording; with
    /// `append` an existing file's scenarios are kept and new exchanges
    /// continue each namespace's numbering after them.
    pub fn create<S: QueuedScenario>(path: PathBuf, append: bool) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create recording directory {}", parent.display())
            })?;
        }

        let state = if append && path.exists() {
            load_recorder_state::<S>(&path)?
        } else {
            RecorderState::default()
        };

        let recorder = Self {
            path,
            state: Mutex::new(state),
        };
        {
            let state = recorder.state.lock().expect("recorder lock");
            recorder
                .flush(&state)
                .context("failed to initialize recording file")?;
        }
        Ok(recorder)
    }

    pub fn push_scenario(&self, namespace: Option<&str>, matcher: Value, script: Value) {
        let mut state = self.state.lock().expect("recorder lock");
        let namespace_label = namespace.unwrap_or("global");
        let sequence = {
            let counter = state
                .counters
                .entry(namespace_label.to_owned())
                .or_insert(0);
            *counter += 1;
            *counter
        };

        let mut scenario = Map::new();
        scenario.insert(
            "scenario_id".to_owned(),
            Value::String(format!("{namespace_label}/{sequence:04}")),
        );
        if let Some(namespace) = namespace {
            scenario.insert("namespace".to_owned(), Value::String(namespace.to_owned()));
        }
        scenario.insert("matcher".to_owned(), matcher);
        scenario.insert("script".to_owned(), script);
        state.scenarios.push(Value::Object(scenario));
        tracing::info!("recorded proxy exchange");

        if let Err(error) = self.flush(&state) {
            tracing::error!(%error, "failed to write proxy recording");
        }
    }

    fn flush(&self, state: &RecorderState) -> Result<()> {
        let mut contents = serde_json::to_string_pretty(&json!({ "scenarios": state.scenarios }))
            .context("failed to serialize recording")?;
        contents.push('\n');
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, contents)
            .with_context(|| format!("failed to write recording to {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to move recording into {}", self.path.display()))?;
        Ok(())
    }
}

/// Loads an existing recording so an append run continues it: the scenarios
/// are kept, and each namespace's counter resumes after the highest
/// recorded sequence number.
fn load_recorder_state<S: QueuedScenario>(path: &Path) -> Result<RecorderState> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read existing recording {}", path.display()))?;
    parse_scenarios::<S>(contents.as_bytes())
        .with_context(|| format!("invalid existing recording {}", path.display()))?;
    let document: Value = serde_json::from_str(&contents)
        .with_context(|| format!("existing recording {} is not valid JSON", path.display()))?;
    let scenarios = document
        .get("scenarios")
        .and_then(Value::as_array)
        .cloned()
        .with_context(|| {
            format!(
                "existing recording {} has no scenarios array",
                path.display()
            )
        })?;

    let mut counters: HashMap<String, u64> = HashMap::new();
    for scenario in &scenarios {
        let Some((label, sequence)) = scenario
            .get("scenario_id")
            .and_then(Value::as_str)
            .and_then(|id| id.rsplit_once('/'))
        else {
            continue;
        };
        let Ok(sequence) = sequence.parse::<u64>() else {
            continue;
        };
        let counter = counters.entry(label.to_owned()).or_insert(0);
        *counter = (*counter).max(sequence);
    }

    Ok(RecorderState {
        scenarios,
        counters,
    })
}
