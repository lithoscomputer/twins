use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::failures::{
    ErrorOutcome, ExecutionOutcome, SuccessOutcome, TranscriptBody, TranscriptOutcome,
    TransportOptions,
};
use super::plan::{ResponsePlan, TokenUsage, ToolCallPlan};
use crate::openai::models::{ChatCompletionsRequest, ResponsesRequest};
use crate::transport::{RawChunk, RawOutcome};

use twin_core::scenario::{validate_response, validate_scenarios, QueuedScenario};
pub use twin_core::scenario::{RequestContext, ScenarioMatcher};
pub type ScenarioEnvelope = twin_core::scenario::ScenarioEnvelope<Scenario>;

#[derive(Clone, Debug, Deserialize)]
pub struct Scenario {
    pub scenario_id: Option<String>,
    /// Restricts startup-template seeding to one bearer-token namespace.
    /// Scenarios without a namespace seed every namespace. Recordings from
    /// proxy-record mode set this to the recording test's bearer token.
    pub namespace: Option<String>,
    pub matcher: ScenarioMatcher,
    pub script: ScenarioScript,
    /// How many matching requests this scenario answers before it is spent.
    /// The default is one. A client that retries a scripted failure needs
    /// one answer per attempt, and `repeat` says how many without copying
    /// the scenario.
    #[serde(default = "default_repeat")]
    pub repeat: u32,
    /// Answers every matching request until the namespace is reset, and is
    /// never spent. `repeat` is ignored when this is set.
    #[serde(default)]
    pub sticky: bool,
}

fn default_repeat() -> u32 {
    1
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioScript {
    Success {
        response_text: Option<String>,
        reasoning: Option<Vec<String>>,
        structured_output: Option<Value>,
        tool_calls: Option<Vec<ToolCallTemplate>>,
        usage: Option<TokenUsage>,
        /// How the response ended. Defaults to a natural stop, or to a tool
        /// call when the script carries tool calls. `length` renders an
        /// incomplete response cut off by the output token limit, so a client
        /// can be tested against a truncated answer.
        finish_reason: Option<FinishReason>,
        delay_before_headers_ms: Option<u64>,
        inter_event_delay_ms: Option<u64>,
        close_after_chunks: Option<usize>,
        malformed_sse: Option<bool>,
    },
    Error {
        status: u16,
        message: String,
        error_type: String,
        code: String,
        retry_after: Option<String>,
        delay_before_headers_ms: Option<u64>,
    },
    Hang {
        delay_before_headers_ms: Option<u64>,
    },
    /// A verbatim recorded exchange, replayed without the canonical engine:
    /// the recorded JSON body or SSE events go back exactly as captured,
    /// provider extension fields and event granularity included.
    Transcript {
        status: u16,
        content_type: Option<String>,
        body: Option<Value>,
        events: Option<Vec<TranscriptEvent>>,
    },
    /// An exact response body for transport-level client tests.
    Raw {
        status: u16,
        content_type: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        chunks: Vec<RawChunk>,
        delay_before_headers_ms: Option<u64>,
    },
}

/// One recorded SSE event of a transcript scenario.
pub use twin_core::sse::TranscriptEvent;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCallTemplate {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_arguments: Option<String>,
    /// A `custom` tool call carries free-form text rather than JSON
    /// arguments, as `custom_tool_call` on the Responses API. `arguments`
    /// then holds that text as a JSON string. Chat Completions has no such
    /// item, so a custom call on that surface renders as a function call.
    #[serde(
        default,
        rename = "kind",
        skip_serializing_if = "ToolCallKind::is_function"
    )]
    pub kind: ToolCallKind,
}

/// Which shape a scripted tool call takes on the wire.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    #[default]
    Function,
    Custom,
}

impl ToolCallKind {
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's skip_serializing_if passes a reference"
    )]
    fn is_function(&self) -> bool {
        *self == Self::Function
    }
}

/// How a scripted success ended.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model stopped on its own, or asked for tools.
    #[default]
    Stop,
    /// The output token limit cut the answer off. Renders as an
    /// `incomplete` response with `max_output_tokens` on the Responses API
    /// and `finish_reason: length` on Chat Completions.
    Length,
}

impl ScenarioScript {
    pub fn script_kind(&self) -> &str {
        match self {
            Self::Success { .. } => "success",
            Self::Error { .. } => "error",
            Self::Hang { .. } => "hang",
            Self::Transcript { .. } => "transcript",
            Self::Raw { .. } => "raw",
        }
    }
}

impl Scenario {
    pub fn matches(&self, request: &RequestContext) -> bool {
        self.matcher.matches(request)
    }

    pub fn execute_for_responses(
        &self,
        response_number: u64,
        request: &ResponsesRequest,
    ) -> ExecutionOutcome {
        match &self.script {
            ScenarioScript::Success {
                response_text,
                reasoning,
                structured_output,
                tool_calls,
                usage,
                finish_reason,
                delay_before_headers_ms,
                inter_event_delay_ms,
                close_after_chunks,
                malformed_sse,
            } => ExecutionOutcome::Success(SuccessOutcome {
                plan: build_plan_from_script(
                    response_number,
                    request.model.clone(),
                    &request.extract_user_text(),
                    response_text.clone(),
                    reasoning.clone().unwrap_or_default(),
                    structured_output.clone(),
                    tool_calls.clone().unwrap_or_default(),
                    *usage,
                    finish_reason.unwrap_or_default(),
                ),
                transport: TransportOptions {
                    delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
                    inter_event_delay_ms: inter_event_delay_ms.unwrap_or_default(),
                    close_after_chunks: *close_after_chunks,
                    malformed_sse: malformed_sse.unwrap_or(false),
                },
            }),
            ScenarioScript::Transcript {
                status,
                content_type,
                body,
                events,
            } => ExecutionOutcome::Transcript(transcript_outcome(
                *status,
                content_type.clone(),
                body.clone(),
                events.clone(),
            )),
            ScenarioScript::Raw {
                status,
                content_type,
                headers,
                chunks,
                delay_before_headers_ms,
            } => ExecutionOutcome::Raw(raw_outcome(
                *status,
                content_type.clone(),
                headers.clone(),
                chunks.clone(),
                *delay_before_headers_ms,
            )),
            ScenarioScript::Error {
                status,
                message,
                error_type,
                code,
                retry_after,
                delay_before_headers_ms,
            } => ExecutionOutcome::Error(ErrorOutcome::new(
                StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                message.clone(),
                error_type.clone(),
                code.clone(),
                retry_after.clone(),
                delay_before_headers_ms.unwrap_or_default(),
            )),
            ScenarioScript::Hang {
                delay_before_headers_ms,
            } => ExecutionOutcome::Hang {
                delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
            },
        }
    }

    pub fn execute_for_chat(
        &self,
        response_number: u64,
        request: &ChatCompletionsRequest,
    ) -> ExecutionOutcome {
        match &self.script {
            ScenarioScript::Success {
                response_text,
                reasoning,
                structured_output,
                tool_calls,
                usage,
                finish_reason,
                delay_before_headers_ms,
                inter_event_delay_ms,
                close_after_chunks,
                malformed_sse,
            } => ExecutionOutcome::Success(SuccessOutcome {
                plan: build_plan_from_script(
                    response_number,
                    request.model.clone(),
                    &request.extract_user_text(),
                    response_text.clone(),
                    reasoning.clone().unwrap_or_default(),
                    structured_output.clone(),
                    tool_calls.clone().unwrap_or_default(),
                    *usage,
                    finish_reason.unwrap_or_default(),
                ),
                transport: TransportOptions {
                    delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
                    inter_event_delay_ms: inter_event_delay_ms.unwrap_or_default(),
                    close_after_chunks: *close_after_chunks,
                    malformed_sse: malformed_sse.unwrap_or(false),
                },
            }),
            ScenarioScript::Transcript {
                status,
                content_type,
                body,
                events,
            } => ExecutionOutcome::Transcript(transcript_outcome(
                *status,
                content_type.clone(),
                body.clone(),
                events.clone(),
            )),
            ScenarioScript::Raw {
                status,
                content_type,
                headers,
                chunks,
                delay_before_headers_ms,
            } => ExecutionOutcome::Raw(raw_outcome(
                *status,
                content_type.clone(),
                headers.clone(),
                chunks.clone(),
                *delay_before_headers_ms,
            )),
            ScenarioScript::Error {
                status,
                message,
                error_type,
                code,
                retry_after,
                delay_before_headers_ms,
            } => ExecutionOutcome::Error(ErrorOutcome::new(
                StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                message.clone(),
                error_type.clone(),
                code.clone(),
                retry_after.clone(),
                delay_before_headers_ms.unwrap_or_default(),
            )),
            ScenarioScript::Hang {
                delay_before_headers_ms,
            } => ExecutionOutcome::Hang {
                delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
            },
        }
    }
}

pub fn validate_scenario_ids<'a>(
    scenarios: impl IntoIterator<Item = &'a Scenario>,
) -> Result<(), String> {
    validate_scenarios(scenarios)
}

impl QueuedScenario for Scenario {
    fn scenario_id(&self) -> Option<&str> {
        self.scenario_id.as_deref()
    }
    fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
    fn matcher(&self) -> &ScenarioMatcher {
        &self.matcher
    }
    fn repeat(&self) -> u32 {
        self.repeat
    }
    fn repeat_mut(&mut self) -> &mut u32 {
        &mut self.repeat
    }
    fn sticky(&self) -> bool {
        self.sticky
    }
    fn script_kind(&self) -> &str {
        self.script.script_kind()
    }

    fn validate(&self) -> Result<(), String> {
        match &self.script {
            ScenarioScript::Raw {
                status,
                headers,
                content_type,
                ..
            } => validate_response(*status, headers, content_type.as_deref(), None),
            ScenarioScript::Transcript {
                status,
                content_type,
                ..
            } => validate_response(*status, &BTreeMap::new(), content_type.as_deref(), None),
            ScenarioScript::Error {
                status,
                retry_after,
                ..
            } => validate_response(*status, &BTreeMap::new(), None, retry_after.as_deref()),
            ScenarioScript::Success { .. } | ScenarioScript::Hang { .. } => Ok(()),
        }
    }
}

fn transcript_outcome(
    status: u16,
    content_type: Option<String>,
    body: Option<Value>,
    events: Option<Vec<TranscriptEvent>>,
) -> TranscriptOutcome {
    TranscriptOutcome {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        content_type,
        body: match events {
            Some(events) => TranscriptBody::Events(events),
            None => TranscriptBody::Json(body.unwrap_or(Value::Null)),
        },
    }
}

fn raw_outcome(
    status: u16,
    content_type: Option<String>,
    headers: BTreeMap<String, String>,
    chunks: Vec<RawChunk>,
    delay_before_headers_ms: Option<u64>,
) -> RawOutcome {
    RawOutcome {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        content_type,
        headers,
        chunks,
        delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "One argument per script field keeps the two call sites readable."
)]
fn build_plan_from_script(
    response_number: u64,
    model: String,
    default_input: &str,
    response_text: Option<String>,
    reasoning: Vec<String>,
    structured_output: Option<Value>,
    tool_calls: Vec<ToolCallTemplate>,
    usage: Option<TokenUsage>,
    finish_reason: FinishReason,
) -> ResponsePlan {
    let output_text = match response_text {
        Some(response_text) => response_text,
        None if tool_calls.is_empty() && structured_output.is_none() => {
            format!("deterministic: {default_input}")
        }
        None => String::new(),
    };
    ResponsePlan {
        id: format!("resp_{response_number:06}"),
        created: response_number,
        model,
        response_text: output_text,
        structured_output,
        reasoning,
        tool_calls: tool_calls
            .into_iter()
            .enumerate()
            .map(|(index, tool_call)| ToolCallPlan {
                id: tool_call
                    .id
                    .unwrap_or_else(|| format!("call_{response_number}_{index}")),
                name: tool_call.name,
                arguments: tool_call.arguments,
                raw_arguments: tool_call.raw_arguments,
                custom: tool_call.kind == ToolCallKind::Custom,
            })
            .collect(),
        usage: usage.unwrap_or_default(),
        truncated: finish_reason == FinishReason::Length,
    }
}
