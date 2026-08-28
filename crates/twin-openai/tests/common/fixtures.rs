//! Scenario fixture derivation for recorded live exchanges.
//!
//! When `TWIN_OPENAI_RECORD_FIXTURES=1`, the live snapshot suite passes every
//! captured exchange through [`write_fixtures`], which maps each turn into a
//! scripted scenario (via `twin_openai::record`) and rewrites
//! `fixtures/scenarios.json`. The offline replay suite loads that file
//! through `TWIN_OPENAI_SCENARIOS_PATH` handling in the twin and replays
//! every case in strict fixture mode.
//!
//! Fixtures keep the genuinely captured content (response text, structured
//! output, tool arguments, usage). Snapshots redact it; see
//! `common::normalize`.
//!
//! Raw transcripts are also written to `fixtures/raw/` (gitignored) to help
//! debug a confusing diff.

use std::fs;
use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use twin_openai::record::{
    derive_script, ExchangeShape, RecordedEndpoint, RecordedExchange, RecordedSseEvent,
};

use super::cases::{marker, turn2_marker, ContractCase, Endpoint};
use super::normalize::CanonicalExchange;
use super::RawExchange;

pub const RECORD_FIXTURES_ENV: &str = "TWIN_OPENAI_RECORD_FIXTURES";

/// One captured request/response turn for a contract case.
pub struct RecordedTurn {
    /// Also the derived scenario's `scenario_id`.
    pub snapshot_name: String,
    pub request: Value,
    pub canonical: CanonicalExchange,
    pub raw: RawExchange,
}

pub fn recording_enabled() -> bool {
    std::env::var(RECORD_FIXTURES_ENV).is_ok_and(|value| value == "1" || value == "true")
}

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

pub fn scenarios_path() -> PathBuf {
    fixtures_dir().join("scenarios.json")
}

fn raw_dir() -> PathBuf {
    fixtures_dir().join("raw")
}

pub fn write_fixtures(recorded: &[(ContractCase, Vec<RecordedTurn>)]) -> Result<()> {
    let mut scenarios = Vec::new();
    for (case, turns) in recorded {
        for (turn_index, turn) in turns.iter().enumerate() {
            scenarios
                .push(derive_scenario(case, turn_index, turn).with_context(|| {
                    format!("failed to derive scenario {}", turn.snapshot_name)
                })?);
        }
    }

    fs::create_dir_all(fixtures_dir()).context("failed to create fixtures directory")?;
    let mut contents = serde_json::to_string_pretty(&json!({ "scenarios": scenarios }))
        .context("failed to serialize scenario fixtures")?;
    contents.push('\n');
    fs::write(scenarios_path(), contents).context("failed to write scenario fixtures")?;

    write_raw_transcripts(recorded)?;
    Ok(())
}

fn derive_scenario(case: &ContractCase, turn_index: usize, turn: &RecordedTurn) -> Result<Value> {
    let input_contains = if turn_index == 0 {
        marker(case.id)
    } else {
        turn2_marker(case.id)
    };
    ensure!(
        turn.request.to_string().contains(&input_contains),
        "request for {} does not contain its scenario marker {input_contains}",
        turn.snapshot_name,
    );

    let shape = ExchangeShape {
        endpoint: recorded_endpoint(case.endpoint),
        stream: case.stream,
        structured: case.structured,
    };
    let exchange = match &turn.raw {
        RawExchange::Json(body) => RecordedExchange::Json(body.clone()),
        RawExchange::Stream(events) => RecordedExchange::Stream(
            events
                .iter()
                .map(|event| RecordedSseEvent {
                    event: event.event.clone(),
                    data: event.data.clone(),
                })
                .collect(),
        ),
    };
    let script = derive_script(shape, &exchange)?;

    Ok(json!({
        "scenario_id": turn.snapshot_name,
        "matcher": {
            "endpoint": case.endpoint.scenario_endpoint(),
            "stream": case.stream,
            "input_contains": input_contains,
        },
        "script": Value::Object(script),
    }))
}

fn recorded_endpoint(endpoint: Endpoint) -> RecordedEndpoint {
    match endpoint {
        Endpoint::Responses => RecordedEndpoint::Responses,
        Endpoint::ChatCompletions => RecordedEndpoint::ChatCompletions,
    }
}

fn write_raw_transcripts(recorded: &[(ContractCase, Vec<RecordedTurn>)]) -> Result<()> {
    let raw_dir = raw_dir();
    if raw_dir.exists() {
        fs::remove_dir_all(&raw_dir).context("failed to clear raw fixtures directory")?;
    }
    fs::create_dir_all(&raw_dir).context("failed to create raw fixtures directory")?;

    for (_, turns) in recorded {
        for turn in turns {
            let exchange = match &turn.raw {
                RawExchange::Json(body) => json!({ "body": body }),
                RawExchange::Stream(events) => {
                    let events: Vec<Value> = events
                        .iter()
                        .map(|event| json!({ "event": event.event, "data": event.data }))
                        .collect();
                    json!({ "events": events })
                }
            };
            let record = json!({
                "request": turn.request,
                "exchange": exchange,
            });
            let mut contents = serde_json::to_string_pretty(&record)
                .context("failed to serialize raw transcript")?;
            contents.push('\n');
            fs::write(
                raw_dir.join(format!("{}.json", turn.snapshot_name)),
                contents,
            )
            .with_context(|| {
                format!("failed to write raw transcript for {}", turn.snapshot_name)
            })?;
        }
    }

    Ok(())
}
