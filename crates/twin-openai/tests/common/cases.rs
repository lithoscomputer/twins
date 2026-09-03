//! Shared registry of contract cases.
//!
//! Each case is captured from the live OpenAI API by the snapshot test in
//! `live_openai_contract.rs` and replayed through the twin by
//! `replay_contract.rs`. Both suites assert the same named snapshots, so the
//! request bodies are defined exactly once here.
//!
//! Every prompt embeds its case marker (see [`marker`]). Derived scenarios
//! match on that marker via `input_contains`, so replay routing never depends
//! on request ordering.

use serde_json::{json, Value};

pub const DEFAULT_MODEL: &str = "gpt-5-nano-2025-08-07";
pub const MODEL_ENV: &str = "TWIN_OPENAI_LIVE_MODEL";
pub const IMAGE_URL: &str = "https://upload.wikimedia.org/wikipedia/commons/thumb/a/a7/React-icon.svg/120px-React-icon.svg.png";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endpoint {
    Responses,
    ChatCompletions,
}

impl Endpoint {
    pub fn path(self) -> &'static str {
        match self {
            Self::Responses => "/v1/responses",
            Self::ChatCompletions => "/v1/chat/completions",
        }
    }

    pub fn scenario_endpoint(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat.completions",
        }
    }
}

#[derive(Clone)]
pub struct ContractCase {
    pub id: &'static str,
    pub endpoint: Endpoint,
    pub stream: bool,
    /// Structured-output cases parse generated JSON in canonical form instead
    /// of redacting it as free text.
    pub structured: bool,
    pub build_request: fn(&str) -> Value,
    /// Optional second-turn request built from the first turn's parsed
    /// response body. The first turn must be non-stream.
    pub follow_up: Option<fn(&str, &Value) -> Value>,
}

pub fn case_model() -> String {
    std::env::var(MODEL_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_owned())
}

pub fn marker(case_id: &str) -> String {
    format!("[case:{case_id}]")
}

pub fn turn2_marker(case_id: &str) -> String {
    format!("[case:{case_id}:turn2]")
}

pub fn structured_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "message": { "type": "string" },
            "ok": { "type": "boolean" }
        },
        "required": ["message", "ok"],
        "additionalProperties": false
    })
}

pub fn weather_tool_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "city": { "type": "string" }
        },
        "required": ["city"],
        "additionalProperties": false
    })
}

pub fn all_cases() -> Vec<ContractCase> {
    vec![
        ContractCase {
            id: "responses_text",
            endpoint: Endpoint::Responses,
            stream: false,
            structured: false,
            build_request: responses_text_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_text_stream",
            endpoint: Endpoint::Responses,
            stream: true,
            structured: false,
            build_request: responses_text_stream_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_structured",
            endpoint: Endpoint::Responses,
            stream: false,
            structured: true,
            build_request: responses_structured_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_structured_stream",
            endpoint: Endpoint::Responses,
            stream: true,
            structured: true,
            build_request: responses_structured_stream_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_tool_call",
            endpoint: Endpoint::Responses,
            stream: false,
            structured: false,
            build_request: responses_tool_call_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_tool_call_stream",
            endpoint: Endpoint::Responses,
            stream: true,
            structured: false,
            build_request: responses_tool_call_stream_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_tool_choice_none",
            endpoint: Endpoint::Responses,
            stream: false,
            structured: false,
            build_request: responses_tool_choice_none_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_image_input",
            endpoint: Endpoint::Responses,
            stream: false,
            structured: false,
            build_request: responses_image_input_request,
            follow_up: None,
        },
        ContractCase {
            id: "responses_continuation",
            endpoint: Endpoint::Responses,
            stream: false,
            structured: false,
            build_request: responses_continuation_request,
            follow_up: Some(responses_continuation_follow_up),
        },
        ContractCase {
            id: "chat_text",
            endpoint: Endpoint::ChatCompletions,
            stream: false,
            structured: false,
            build_request: chat_text_request,
            follow_up: None,
        },
        ContractCase {
            id: "chat_text_stream",
            endpoint: Endpoint::ChatCompletions,
            stream: true,
            structured: false,
            build_request: chat_text_stream_request,
            follow_up: None,
        },
        ContractCase {
            id: "chat_structured",
            endpoint: Endpoint::ChatCompletions,
            stream: false,
            structured: true,
            build_request: chat_structured_request,
            follow_up: None,
        },
        ContractCase {
            id: "chat_tool_call",
            endpoint: Endpoint::ChatCompletions,
            stream: false,
            structured: false,
            build_request: chat_tool_call_request,
            follow_up: None,
        },
        ContractCase {
            id: "chat_tool_call_stream",
            endpoint: Endpoint::ChatCompletions,
            stream: true,
            structured: false,
            build_request: chat_tool_call_stream_request,
            follow_up: None,
        },
        ContractCase {
            id: "chat_tool_choice_none",
            endpoint: Endpoint::ChatCompletions,
            stream: false,
            structured: false,
            build_request: chat_tool_choice_none_request,
            follow_up: None,
        },
    ]
}

fn responses_text_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_text] Reply with a short greeting.",
        "stream": false
    })
}

fn responses_text_stream_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_text_stream] Reply with a short greeting.",
        "stream": true
    })
}

fn responses_structured_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_structured] Return a short structured answer.",
        "stream": false,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "case_schema",
                "schema": structured_schema(),
                "strict": true
            }
        }
    })
}

fn responses_structured_stream_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_structured_stream] Return a short structured answer.",
        "stream": true,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "case_schema",
                "schema": structured_schema(),
                "strict": true
            }
        }
    })
}

fn responses_weather_tools() -> Value {
    json!([{
        "type": "function",
        "name": "lookup_weather",
        "description": "Look up the weather",
        "parameters": weather_tool_parameters(),
        "strict": true
    }])
}

fn responses_tool_call_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_tool_call] Use the weather tool for Boston.",
        "stream": false,
        "tools": responses_weather_tools(),
        "tool_choice": {
            "type": "function",
            "name": "lookup_weather"
        }
    })
}

fn responses_tool_call_stream_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_tool_call_stream] Use the weather tool for Boston.",
        "stream": true,
        "tools": responses_weather_tools(),
        "tool_choice": {
            "type": "function",
            "name": "lookup_weather"
        }
    })
}

fn responses_tool_choice_none_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_tool_choice_none] Describe the weather tool without calling it.",
        "stream": false,
        "tools": responses_weather_tools(),
        "tool_choice": "none"
    })
}

fn responses_image_input_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": [{
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": "[case:responses_image_input] Describe this image in a few words."
                },
                {
                    "type": "input_image",
                    "image_url": IMAGE_URL
                }
            ]
        }],
        "stream": false
    })
}

fn responses_continuation_request(model: &str) -> Value {
    json!({
        "model": model,
        "input": "[case:responses_continuation] Reply with a short greeting.",
        "stream": false
    })
}

fn responses_continuation_follow_up(model: &str, first_response: &Value) -> Value {
    let previous_response_id = first_response
        .get("id")
        .and_then(Value::as_str)
        .expect("first continuation turn should include a response id");
    json!({
        "model": model,
        "previous_response_id": previous_response_id,
        "input": "[case:responses_continuation:turn2] Reply with a short farewell.",
        "stream": false
    })
}

fn chat_text_request(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "[case:chat_text] Reply with a short greeting."
        }],
        "stream": false
    })
}

fn chat_text_stream_request(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "[case:chat_text_stream] Reply with a short greeting."
        }],
        "stream": true,
        "stream_options": { "include_usage": true }
    })
}

fn chat_structured_request(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "[case:chat_structured] Return a short structured answer."
        }],
        "stream": false,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "case_schema",
                "schema": structured_schema(),
                "strict": true
            }
        }
    })
}

fn chat_weather_tools() -> Value {
    json!([{
        "type": "function",
        "function": {
            "name": "lookup_weather",
            "description": "Look up the weather",
            "parameters": weather_tool_parameters(),
            "strict": true
        }
    }])
}

fn chat_tool_call_request(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "[case:chat_tool_call] Use the weather tool for Boston."
        }],
        "stream": false,
        "tools": chat_weather_tools(),
        "tool_choice": {
            "type": "function",
            "function": { "name": "lookup_weather" }
        }
    })
}

fn chat_tool_call_stream_request(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "[case:chat_tool_call_stream] Use the weather tool for Boston."
        }],
        "stream": true,
        "stream_options": { "include_usage": true },
        "tools": chat_weather_tools(),
        "tool_choice": {
            "type": "function",
            "function": { "name": "lookup_weather" }
        }
    })
}

fn chat_tool_choice_none_request(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "[case:chat_tool_choice_none] Describe the weather tool without calling it."
        }],
        "stream": false,
        "tools": chat_weather_tools(),
        "tool_choice": "none"
    })
}
