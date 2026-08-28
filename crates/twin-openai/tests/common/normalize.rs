//! Canonicalization shared by the live snapshot suite and the offline replay
//! suite.
//!
//! Live OpenAI responses and twin responses are both projected onto the
//! contract documented in `docs/compatibility-matrix.md`:
//!
//! - volatile values (ids, timestamps, token counts) are redacted,
//! - free-form generated text collapses to `"[text]"`,
//! - generated JSON (structured output, tool arguments) is parsed so
//!   formatting is not part of the contract,
//! - stream chunk boundaries collapse into a deduplicated milestone sequence,
//! - fields and events outside the contract are dropped from the canonical
//!   value and recorded in a sorted `extras` inventory instead.
//!
//! The canonical value is asserted against the same named snapshot by both
//! suites. The extras inventory is asserted only by the live suite, so drift
//! in fields outside the contract still shows up in nightly diffs without
//! breaking replay parity.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use super::cases::{ContractCase, Endpoint};
use super::ParsedSseTranscript;

pub struct CanonicalExchange {
    pub canonical: Value,
    /// Sorted inventory of observed fields and events outside the canonical
    /// contract.
    pub extras: Vec<String>,
}

/// Assert a named insta snapshot with the settings shared by the live and
/// replay suites. Both suites must resolve to the same snapshot file, so the
/// module prefix is dropped from the file name.
///
/// Returns an error instead of panicking so a suite can assert every case and
/// report all mismatches at once.
pub fn assert_named_snapshot(name: &str, value: &Value) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        insta::with_settings!({ prepend_module_to_snapshot => false, omit_expression => true }, {
            insta::assert_json_snapshot!(name, value);
        });
    }))
    .map_err(|_| format!("snapshot mismatch: {name}"))
}

#[derive(Default)]
struct Extras(BTreeSet<String>);

impl Extras {
    fn note(&mut self, path: String) {
        self.0.insert(path);
    }

    fn into_vec(self) -> Vec<String> {
        self.0.into_iter().collect()
    }
}

pub fn canonical_json_exchange(
    case: &ContractCase,
    status: u16,
    content_type: Option<&str>,
    body: &Value,
) -> CanonicalExchange {
    let mut extras = Extras::default();
    let body = match case.endpoint {
        Endpoint::Responses => canonical_responses_body(case, body, &mut extras),
        Endpoint::ChatCompletions => canonical_chat_body(case, body, &mut extras),
    };

    CanonicalExchange {
        canonical: json!({
            "status": status,
            "content_type": canonical_content_type(content_type),
            "body": body,
        }),
        extras: extras.into_vec(),
    }
}

pub fn canonical_stream_exchange(
    case: &ContractCase,
    status: u16,
    content_type: Option<&str>,
    transcript: &ParsedSseTranscript,
) -> CanonicalExchange {
    match case.endpoint {
        Endpoint::Responses => canonical_responses_stream(case, status, content_type, transcript),
        Endpoint::ChatCompletions => canonical_chat_stream(case, status, content_type, transcript),
    }
}

fn canonical_content_type(value: Option<&str>) -> Value {
    match value {
        Some(value) => Value::String(
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned(),
        ),
        None => Value::Null,
    }
}

fn canonical_responses_body(case: &ContractCase, body: &Value, extras: &mut Extras) -> Value {
    const KNOWN: [&str; 6] = ["id", "object", "model", "status", "output", "usage"];

    let Some(object) = body.as_object() else {
        extras.note("body.not_an_object".to_owned());
        return Value::Null;
    };

    note_unknown_keys(object, &KNOWN, "body", extras);

    let output = object
        .get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| canonical_output_item(case, item, extras))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut canonical = Map::new();
    canonical.insert("id".to_owned(), redact_id(object.get("id")));
    canonical.insert("object".to_owned(), copy_value(object.get("object")));
    canonical.insert("model".to_owned(), copy_value(object.get("model")));
    canonical.insert("status".to_owned(), copy_value(object.get("status")));
    canonical.insert("output".to_owned(), Value::Array(output));
    canonical.insert(
        "usage".to_owned(),
        canonical_usage(
            object.get("usage"),
            &["input_tokens", "output_tokens", "total_tokens"],
            "body.usage",
            extras,
        ),
    );
    Value::Object(canonical)
}

fn canonical_output_item(case: &ContractCase, item: &Value, extras: &mut Extras) -> Option<Value> {
    let Some(object) = item.as_object() else {
        extras.note("body.output[not_an_object]".to_owned());
        return None;
    };

    match object.get("type").and_then(Value::as_str) {
        Some("message") => Some(canonical_message_item(case, object, extras)),
        Some("function_call") => Some(canonical_function_call_item(object, extras)),
        // Reasoning output items are outside the canonical contract; the twin
        // does not model reasoning summaries.
        Some(kind) => {
            extras.note(format!("body.output[type={kind}]"));
            None
        }
        None => {
            extras.note("body.output[missing_type]".to_owned());
            None
        }
    }
}

fn canonical_message_item(
    case: &ContractCase,
    object: &Map<String, Value>,
    extras: &mut Extras,
) -> Value {
    const KNOWN: [&str; 4] = ["id", "type", "role", "content"];
    note_unknown_keys(object, &KNOWN, "body.output.message", extras);

    let content = object
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| canonical_content_part(case, part, extras))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    json!({
        "id": redact_id(object.get("id")),
        "type": "message",
        "role": copy_value(object.get("role")),
        "content": content,
    })
}

fn canonical_content_part(case: &ContractCase, part: &Value, extras: &mut Extras) -> Option<Value> {
    let Some(object) = part.as_object() else {
        extras.note("body.output.message.content[not_an_object]".to_owned());
        return None;
    };

    match object.get("type").and_then(Value::as_str) {
        Some("output_text") => {
            note_unknown_keys(
                object,
                &["type", "text"],
                "body.output.message.content.output_text",
                extras,
            );
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if case.structured {
                Some(json!({
                    "type": "structured",
                    "json": parse_generated_json(text),
                }))
            } else {
                Some(json!({
                    "type": "output_text",
                    "text": redact_text(text),
                }))
            }
        }
        Some("output_json") => {
            note_unknown_keys(
                object,
                &["type", "json"],
                "body.output.message.content.output_json",
                extras,
            );
            Some(json!({
                "type": "structured",
                "json": copy_value(object.get("json")),
            }))
        }
        Some(kind) => {
            extras.note(format!("body.output.message.content[type={kind}]"));
            None
        }
        None => {
            extras.note("body.output.message.content[missing_type]".to_owned());
            None
        }
    }
}

fn canonical_function_call_item(object: &Map<String, Value>, extras: &mut Extras) -> Value {
    const KNOWN: [&str; 5] = ["id", "type", "call_id", "name", "arguments"];
    note_unknown_keys(object, &KNOWN, "body.output.function_call", extras);

    json!({
        "id": redact_id(object.get("id")),
        "type": "function_call",
        "call_id": redact_id(object.get("call_id")),
        "name": copy_value(object.get("name")),
        "arguments": canonical_arguments(object.get("arguments")),
    })
}

fn canonical_chat_body(case: &ContractCase, body: &Value, extras: &mut Extras) -> Value {
    const KNOWN: [&str; 6] = ["id", "object", "created", "model", "choices", "usage"];

    let Some(object) = body.as_object() else {
        extras.note("body.not_an_object".to_owned());
        return Value::Null;
    };

    note_unknown_keys(object, &KNOWN, "body", extras);

    let choices = object
        .get("choices")
        .and_then(Value::as_array)
        .map(|choices| {
            choices
                .iter()
                .map(|choice| canonical_chat_choice(case, choice, extras))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    json!({
        "id": redact_id(object.get("id")),
        "object": copy_value(object.get("object")),
        "created": redact_int(object.get("created"), "[timestamp]"),
        "model": copy_value(object.get("model")),
        "choices": choices,
        "usage": canonical_usage(
            object.get("usage"),
            &["prompt_tokens", "completion_tokens", "total_tokens"],
            "body.usage",
            extras,
        ),
    })
}

fn canonical_chat_choice(case: &ContractCase, choice: &Value, extras: &mut Extras) -> Value {
    const KNOWN: [&str; 3] = ["index", "finish_reason", "message"];

    let Some(object) = choice.as_object() else {
        extras.note("body.choices[not_an_object]".to_owned());
        return Value::Null;
    };

    note_unknown_keys(object, &KNOWN, "body.choices", extras);

    json!({
        "index": copy_value(object.get("index")),
        "finish_reason": copy_value(object.get("finish_reason")),
        "message": canonical_chat_message(case, object.get("message"), extras),
    })
}

fn canonical_chat_message(
    case: &ContractCase,
    message: Option<&Value>,
    extras: &mut Extras,
) -> Value {
    const KNOWN: [&str; 3] = ["role", "content", "tool_calls"];

    let Some(object) = message.and_then(Value::as_object) else {
        extras.note("body.choices.message[not_an_object]".to_owned());
        return Value::Null;
    };

    note_unknown_keys(object, &KNOWN, "body.choices.message", extras);

    let mut canonical = Map::new();
    canonical.insert("role".to_owned(), copy_value(object.get("role")));
    canonical.insert(
        "content".to_owned(),
        canonical_chat_content(case, object.get("content")),
    );

    let tool_calls = object
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|tool_calls| {
            tool_calls
                .iter()
                .map(|tool_call| canonical_chat_tool_call(tool_call, extras))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !tool_calls.is_empty() {
        canonical.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }

    Value::Object(canonical)
}

fn canonical_chat_content(case: &ContractCase, content: Option<&Value>) -> Value {
    match content {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) if text.is_empty() => Value::Null,
        Some(Value::String(text)) if case.structured => json!({
            "json": parse_generated_json(text),
        }),
        Some(Value::String(_)) => Value::String("[text]".to_owned()),
        Some(other) => other.clone(),
    }
}

fn canonical_chat_tool_call(tool_call: &Value, extras: &mut Extras) -> Value {
    const KNOWN: [&str; 3] = ["id", "type", "function"];

    let Some(object) = tool_call.as_object() else {
        extras.note("body.choices.message.tool_calls[not_an_object]".to_owned());
        return Value::Null;
    };

    note_unknown_keys(object, &KNOWN, "body.choices.message.tool_calls", extras);

    let function = object.get("function").and_then(Value::as_object);
    if let Some(function) = function {
        note_unknown_keys(
            function,
            &["name", "arguments"],
            "body.choices.message.tool_calls.function",
            extras,
        );
    }

    json!({
        "id": redact_id(object.get("id")),
        "type": copy_value(object.get("type")),
        "function": {
            "name": copy_value(function.and_then(|function| function.get("name"))),
            "arguments": canonical_arguments(
                function.and_then(|function| function.get("arguments")),
            ),
        }
    })
}

fn canonical_responses_stream(
    case: &ContractCase,
    status: u16,
    content_type: Option<&str>,
    transcript: &ParsedSseTranscript,
) -> CanonicalExchange {
    let mut extras = Extras::default();
    let mut milestones: Vec<String> = Vec::new();
    let mut completed = Value::Null;

    for event in &transcript.events {
        let Some(name) = event.event.as_deref() else {
            if event.data == "[DONE]" {
                push_milestone(&mut milestones, "done".to_owned());
            } else {
                extras.note("event[unnamed]".to_owned());
            }
            continue;
        };
        let data: Value = serde_json::from_str(&event.data).unwrap_or(Value::Null);

        let milestone = match name {
            "response.created"
            | "response.in_progress"
            | "response.completed"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.delta"
            | "response.output_text.done"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.reasoning.delta" => Some(name.to_owned()),
            "response.output_item.added" | "response.output_item.done" => {
                let kind = data
                    .get("item")
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                Some(format!("{name}[{kind}]"))
            }
            other => {
                extras.note(format!("event.{other}"));
                None
            }
        };
        if let Some(milestone) = milestone {
            push_milestone(&mut milestones, milestone);
        }

        if name == "response.completed" {
            completed = data.get("response").map_or(Value::Null, |response| {
                canonical_responses_body(case, response, &mut extras)
            });
        }
    }

    CanonicalExchange {
        canonical: json!({
            "status": status,
            "content_type": canonical_content_type(content_type),
            "milestones": milestones,
            "completed": completed,
        }),
        extras: extras.into_vec(),
    }
}

fn canonical_chat_stream(
    case: &ContractCase,
    status: u16,
    content_type: Option<&str>,
    transcript: &ParsedSseTranscript,
) -> CanonicalExchange {
    const KNOWN_CHUNK: [&str; 6] = ["id", "object", "created", "model", "choices", "usage"];
    const KNOWN_CHOICE: [&str; 3] = ["index", "delta", "finish_reason"];
    const KNOWN_DELTA: [&str; 4] = ["role", "content", "tool_calls", "reasoning"];

    let mut extras = Extras::default();
    let mut milestones: Vec<String> = Vec::new();
    let mut content = String::new();
    let mut tool_calls: BTreeMap<u64, AssembledToolCall> = BTreeMap::new();
    let mut finish_reason = Value::Null;
    let mut usage = Value::Null;
    let mut done = false;

    for event in &transcript.events {
        if event.data == "[DONE]" {
            push_milestone(&mut milestones, "done".to_owned());
            done = true;
            continue;
        }

        let Ok(chunk) = serde_json::from_str::<Value>(&event.data) else {
            extras.note("chunk[not_json]".to_owned());
            continue;
        };
        let Some(chunk) = chunk.as_object() else {
            extras.note("chunk[not_an_object]".to_owned());
            continue;
        };

        note_unknown_keys(chunk, &KNOWN_CHUNK, "chunk", &mut extras);

        if let Some(chunk_usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            push_milestone(&mut milestones, "usage".to_owned());
            usage = canonical_usage(
                Some(chunk_usage),
                &["prompt_tokens", "completion_tokens", "total_tokens"],
                "chunk.usage",
                &mut extras,
            );
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(Value::as_object)
        else {
            continue;
        };

        note_unknown_keys(choice, &KNOWN_CHOICE, "chunk.choice", &mut extras);

        if let Some(delta) = choice.get("delta").and_then(Value::as_object) {
            note_unknown_keys(delta, &KNOWN_DELTA, "chunk.delta", &mut extras);

            if delta.get("role").and_then(Value::as_str).is_some() {
                push_milestone(&mut milestones, "role".to_owned());
            }
            if let Some(text) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                push_milestone(&mut milestones, "content_delta".to_owned());
                content.push_str(text);
            }
            if delta.get("reasoning").and_then(Value::as_str).is_some() {
                push_milestone(&mut milestones, "reasoning_delta".to_owned());
            }
            if let Some(deltas) = delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .filter(|deltas| !deltas.is_empty())
            {
                push_milestone(&mut milestones, "tool_call_delta".to_owned());
                for tool_call_delta in deltas {
                    assemble_tool_call_delta(&mut tool_calls, tool_call_delta);
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            push_milestone(&mut milestones, format!("finish[{reason}]"));
            finish_reason = Value::String(reason.to_owned());
        }
    }

    let assembled_tool_calls: Vec<Value> = tool_calls
        .into_values()
        .map(|tool_call| {
            json!({
                "name": tool_call.name,
                "arguments": parse_generated_json(&tool_call.arguments),
            })
        })
        .collect();

    let mut assembled = Map::new();
    assembled.insert(
        "content".to_owned(),
        if content.is_empty() {
            Value::Null
        } else if case.structured {
            json!({ "json": parse_generated_json(&content) })
        } else {
            Value::String("[text]".to_owned())
        },
    );
    if !assembled_tool_calls.is_empty() {
        assembled.insert("tool_calls".to_owned(), Value::Array(assembled_tool_calls));
    }
    assembled.insert("finish_reason".to_owned(), finish_reason);
    assembled.insert("usage".to_owned(), usage);

    CanonicalExchange {
        canonical: json!({
            "status": status,
            "content_type": canonical_content_type(content_type),
            "milestones": milestones,
            "assembled": Value::Object(assembled),
            "done": done,
        }),
        extras: extras.into_vec(),
    }
}

#[derive(Default)]
struct AssembledToolCall {
    name: String,
    arguments: String,
}

fn assemble_tool_call_delta(tool_calls: &mut BTreeMap<u64, AssembledToolCall>, delta: &Value) {
    let index = delta.get("index").and_then(Value::as_u64).unwrap_or(0);
    let entry = tool_calls.entry(index).or_default();
    if let Some(function) = delta.get("function").and_then(Value::as_object) {
        if let Some(name) = function.get("name").and_then(Value::as_str) {
            entry.name.push_str(name);
        }
        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
            entry.arguments.push_str(arguments);
        }
    }
}

fn push_milestone(milestones: &mut Vec<String>, milestone: String) {
    if milestones.last() != Some(&milestone) {
        milestones.push(milestone);
    }
}

fn note_unknown_keys(
    object: &Map<String, Value>,
    known: &[&str],
    prefix: &str,
    extras: &mut Extras,
) {
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            extras.note(format!("{prefix}.{key}"));
        }
    }
}

fn copy_value(value: Option<&Value>) -> Value {
    value.cloned().unwrap_or(Value::Null)
}

fn redact_id(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(_)) => Value::String("[id]".to_owned()),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn redact_int(value: Option<&Value>, replacement: &str) -> Value {
    match value {
        Some(Value::Number(_)) => Value::String(replacement.to_owned()),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn redact_text(text: &str) -> Value {
    if text.is_empty() {
        Value::String(String::new())
    } else {
        Value::String("[text]".to_owned())
    }
}

/// Parse model-generated JSON so formatting differences (whitespace, key
/// spacing) are not part of the contract. Unparseable input stays visible.
fn parse_generated_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String("[unparsed]".to_owned()))
}

fn canonical_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => parse_generated_json(text),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn canonical_usage(
    value: Option<&Value>,
    known: &[&str],
    prefix: &str,
    extras: &mut Extras,
) -> Value {
    let Some(object) = value.and_then(Value::as_object) else {
        return Value::Null;
    };

    note_unknown_keys(object, known, prefix, extras);

    let mut canonical = Map::new();
    for key in known {
        canonical.insert((*key).to_owned(), redact_int(object.get(*key), "[int]"));
    }
    Value::Object(canonical)
}
