//! Scenario fixture derivation for recorded live exchanges.
//!
//! When `TWIN_OPENAI_RECORD_FIXTURES=1`, the live snapshot suite passes every
//! captured exchange through [`write_fixtures`], which maps each turn into a
//! scripted scenario and rewrites `fixtures/scenarios.json`. The offline
//! replay suite loads that file through `TWIN_OPENAI_SCENARIOS_PATH` handling
//! in the twin and replays every case in strict fixture mode.
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
use serde_json::{json, Map, Value};

use super::cases::{marker, turn2_marker, ContractCase, Endpoint};
use super::normalize::CanonicalExchange;
use super::{ParsedSseEvent, RawExchange};

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

    let observation = match &turn.raw {
        RawExchange::Json(body) => observe_body(case, body)?,
        RawExchange::Stream(events) => observe_stream(case, events)?,
    };

    let mut script = Map::new();
    script.insert("kind".to_owned(), Value::String("success".to_owned()));
    if let Some(response_text) = observation.response_text {
        script.insert("response_text".to_owned(), Value::String(response_text));
    }
    if let Some(structured_output) = observation.structured_output {
        script.insert("structured_output".to_owned(), structured_output);
    }
    if !observation.tool_calls.is_empty() {
        let tool_calls: Vec<Value> = observation
            .tool_calls
            .into_iter()
            .map(|tool_call| {
                json!({
                    "name": tool_call.name,
                    "arguments": tool_call.arguments,
                })
            })
            .collect();
        script.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    if let Some((input_tokens, output_tokens)) = observation.usage {
        script.insert(
            "usage".to_owned(),
            json!({
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }),
        );
    }

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

#[derive(Default)]
struct Observation {
    response_text: Option<String>,
    structured_output: Option<Value>,
    tool_calls: Vec<ObservedToolCall>,
    usage: Option<(u64, u64)>,
}

struct ObservedToolCall {
    name: String,
    arguments: Value,
}

fn observe_body(case: &ContractCase, body: &Value) -> Result<Observation> {
    match case.endpoint {
        Endpoint::Responses => observe_responses_body(case, body),
        Endpoint::ChatCompletions => observe_chat_body(case, body),
    }
}

fn observe_responses_body(case: &ContractCase, body: &Value) -> Result<Observation> {
    let mut observation = Observation::default();

    let output = body
        .get("output")
        .and_then(Value::as_array)
        .context("responses body did not contain an output array")?;

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .unwrap_or(&Vec::new())
                {
                    observe_responses_content_part(case, part, &mut observation);
                }
            }
            Some("function_call") => {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .context("function_call output item did not contain a name")?;
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .context("function_call output item did not contain arguments")?;
                observation.tool_calls.push(ObservedToolCall {
                    name: name.to_owned(),
                    arguments: parse_arguments(arguments),
                });
            }
            _ => {}
        }
    }

    observation.usage = observe_usage(body.get("usage"), "input_tokens", "output_tokens");
    Ok(observation)
}

fn observe_responses_content_part(
    case: &ContractCase,
    part: &Value,
    observation: &mut Observation,
) {
    match part.get("type").and_then(Value::as_str) {
        Some("output_text") => {
            let Some(text) = part.get("text").and_then(Value::as_str) else {
                return;
            };
            if case.structured {
                if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                    observation.structured_output = Some(parsed);
                    return;
                }
            }
            if !text.is_empty() {
                observation.response_text = Some(text.to_owned());
            }
        }
        Some("output_json") => {
            observation.structured_output = part.get("json").cloned();
        }
        _ => {}
    }
}

fn observe_chat_body(case: &ContractCase, body: &Value) -> Result<Observation> {
    let mut observation = Observation::default();

    let message = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .context("chat body did not contain choices[0].message")?;

    if let Some(content) = message.get("content").and_then(Value::as_str) {
        observe_chat_content(case, content, &mut observation);
    }

    for tool_call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let function = tool_call.get("function");
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .context("chat tool call did not contain a function name")?;
        let arguments = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .context("chat tool call did not contain function arguments")?;
        observation.tool_calls.push(ObservedToolCall {
            name: name.to_owned(),
            arguments: parse_arguments(arguments),
        });
    }

    observation.usage = observe_usage(body.get("usage"), "prompt_tokens", "completion_tokens");
    Ok(observation)
}

fn observe_chat_content(case: &ContractCase, content: &str, observation: &mut Observation) {
    if case.structured {
        if let Ok(parsed) = serde_json::from_str::<Value>(content) {
            observation.structured_output = Some(parsed);
            return;
        }
    }
    if !content.is_empty() {
        observation.response_text = Some(content.to_owned());
    }
}

fn observe_stream(case: &ContractCase, events: &[ParsedSseEvent]) -> Result<Observation> {
    match case.endpoint {
        Endpoint::Responses => observe_responses_stream(case, events),
        Endpoint::ChatCompletions => observe_chat_stream(case, events),
    }
}

fn observe_responses_stream(case: &ContractCase, events: &[ParsedSseEvent]) -> Result<Observation> {
    for event in events {
        if event.event.as_deref() != Some("response.completed") {
            continue;
        }
        let data: Value = serde_json::from_str(&event.data)
            .context("response.completed data was not valid JSON")?;
        let response = data
            .get("response")
            .context("response.completed did not contain a response")?;
        return observe_responses_body(case, response);
    }

    anyhow::bail!("responses stream did not contain a response.completed event");
}

fn observe_chat_stream(case: &ContractCase, events: &[ParsedSseEvent]) -> Result<Observation> {
    let mut observation = Observation::default();
    let mut content = String::new();
    let mut tool_calls: std::collections::BTreeMap<u64, (String, String)> =
        std::collections::BTreeMap::new();

    for event in events {
        if event.data == "[DONE]" {
            continue;
        }
        let chunk: Value =
            serde_json::from_str(&event.data).context("chat chunk was not valid JSON")?;

        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            observation.usage = observe_usage(Some(usage), "prompt_tokens", "completion_tokens");
        }

        let Some(delta) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
        else {
            continue;
        };

        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            content.push_str(text);
        }
        for tool_call_delta in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let index = tool_call_delta
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let entry = tool_calls.entry(index).or_default();
            if let Some(function) = tool_call_delta.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    entry.0.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    entry.1.push_str(arguments);
                }
            }
        }
    }

    if !content.is_empty() {
        observe_chat_content(case, &content, &mut observation);
    }
    observation.tool_calls = tool_calls
        .into_values()
        .map(|(name, arguments)| ObservedToolCall {
            name,
            arguments: parse_arguments(&arguments),
        })
        .collect();

    Ok(observation)
}

fn observe_usage(usage: Option<&Value>, input_key: &str, output_key: &str) -> Option<(u64, u64)> {
    let usage = usage?;
    let input_tokens = usage.get(input_key).and_then(Value::as_u64)?;
    let output_tokens = usage.get(output_key).and_then(Value::as_u64)?;
    Some((input_tokens, output_tokens))
}

/// Tool arguments are stored parsed so replay does not depend on the exact
/// serialization the model produced. Unparseable arguments are kept verbatim
/// as a JSON string.
fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_owned()))
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
