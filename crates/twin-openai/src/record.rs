//! Derivation of scripted scenarios from captured OpenAI exchanges.
//!
//! A captured exchange (a non-stream JSON body or a parsed SSE transcript)
//! is mapped into a `ScenarioScript`-shaped `script` object carrying the
//! genuinely captured content: response text, structured output, parsed tool
//! arguments, and usage. The live snapshot suite uses this to write
//! `fixtures/scenarios.json`, and proxy-record mode uses it to record an
//! application's own traffic.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordedEndpoint {
    Responses,
    ChatCompletions,
}

impl RecordedEndpoint {
    #[must_use]
    pub fn scenario_endpoint(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat.completions",
        }
    }
}

/// The request-side facts derivation needs about an exchange.
#[derive(Clone, Copy, Debug)]
pub struct ExchangeShape {
    pub endpoint: RecordedEndpoint,
    pub stream: bool,
    /// Structured-output exchange: generated JSON is stored as
    /// `structured_output` instead of `response_text`.
    pub structured: bool,
}

impl ExchangeShape {
    /// Derive the shape from a request body.
    #[must_use]
    pub fn from_request(endpoint: RecordedEndpoint, request: &Value) -> Self {
        let stream = request
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let format_kind = match endpoint {
            RecordedEndpoint::Responses => request
                .get("text")
                .and_then(|text| text.get("format"))
                .and_then(|format| format.get("type"))
                .and_then(Value::as_str),
            RecordedEndpoint::ChatCompletions => request
                .get("response_format")
                .and_then(|format| format.get("type"))
                .and_then(Value::as_str),
        };
        let structured = matches!(format_kind, Some("json_object" | "json_schema"));

        Self {
            endpoint,
            stream,
            structured,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecordedSseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// A captured response, ready for derivation.
pub enum RecordedExchange {
    Json(Value),
    Stream(Vec<RecordedSseEvent>),
}

/// Derive the scenario `script` object for a successful exchange.
pub fn derive_script(
    shape: ExchangeShape,
    exchange: &RecordedExchange,
) -> Result<Map<String, Value>> {
    let observation = match exchange {
        RecordedExchange::Json(body) => observe_body(shape, body)?,
        RecordedExchange::Stream(events) => observe_stream(shape, events)?,
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

    Ok(script)
}

/// Parse an SSE body into its events. Tolerates `id:` and comment lines and
/// a missing trailing blank line.
pub fn parse_sse_events(body: &[u8]) -> Result<Vec<RecordedSseEvent>> {
    let text = std::str::from_utf8(body).context("sse body was not valid utf-8")?;
    let mut events = Vec::new();

    for block in text.split("\n\n") {
        if block.trim().is_empty() {
            continue;
        }

        let mut event = None;
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("event: ") {
                event = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("data: ") {
                data_lines.push(value.to_owned());
            }
        }

        if event.is_some() || !data_lines.is_empty() {
            events.push(RecordedSseEvent {
                event,
                data: data_lines.join("\n"),
            });
        }
    }

    Ok(events)
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

fn observe_body(shape: ExchangeShape, body: &Value) -> Result<Observation> {
    match shape.endpoint {
        RecordedEndpoint::Responses => observe_responses_body(shape, body),
        RecordedEndpoint::ChatCompletions => observe_chat_body(shape, body),
    }
}

fn observe_responses_body(shape: ExchangeShape, body: &Value) -> Result<Observation> {
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
                    observe_responses_content_part(shape, part, &mut observation);
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
    shape: ExchangeShape,
    part: &Value,
    observation: &mut Observation,
) {
    match part.get("type").and_then(Value::as_str) {
        Some("output_text") => {
            let Some(text) = part.get("text").and_then(Value::as_str) else {
                return;
            };
            if shape.structured {
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

fn observe_chat_body(shape: ExchangeShape, body: &Value) -> Result<Observation> {
    let mut observation = Observation::default();

    let message = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .context("chat body did not contain choices[0].message")?;

    if let Some(content) = message.get("content").and_then(Value::as_str) {
        observe_chat_content(shape, content, &mut observation);
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

fn observe_chat_content(shape: ExchangeShape, content: &str, observation: &mut Observation) {
    if shape.structured {
        if let Ok(parsed) = serde_json::from_str::<Value>(content) {
            observation.structured_output = Some(parsed);
            return;
        }
    }
    if !content.is_empty() {
        observation.response_text = Some(content.to_owned());
    }
}

fn observe_stream(shape: ExchangeShape, events: &[RecordedSseEvent]) -> Result<Observation> {
    match shape.endpoint {
        RecordedEndpoint::Responses => observe_responses_stream(shape, events),
        RecordedEndpoint::ChatCompletions => observe_chat_stream(shape, events),
    }
}

fn observe_responses_stream(
    shape: ExchangeShape,
    events: &[RecordedSseEvent],
) -> Result<Observation> {
    for event in events {
        if event.event.as_deref() != Some("response.completed") {
            continue;
        }
        let data: Value = serde_json::from_str(&event.data)
            .context("response.completed data was not valid JSON")?;
        let response = data
            .get("response")
            .context("response.completed did not contain a response")?;
        return observe_responses_body(shape, response);
    }

    anyhow::bail!("responses stream did not contain a response.completed event");
}

fn observe_chat_stream(shape: ExchangeShape, events: &[RecordedSseEvent]) -> Result<Observation> {
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
        observe_chat_content(shape, &content, &mut observation);
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
