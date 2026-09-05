use super::plan::TokenUsage;
use crate::transport::{RawChunk, RawOutcome};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use twin_core::scenario::{validate_scenarios, QueuedScenario};
pub use twin_core::scenario::{RequestContext, ScenarioMatcher};
pub type ScenarioEnvelope = twin_core::scenario::ScenarioEnvelope<Scenario>;

#[derive(Clone, Debug, Deserialize)]
pub struct Scenario {
    pub scenario_id: Option<String>,
    pub namespace: Option<String>,
    pub matcher: ScenarioMatcher,
    pub script: ScenarioScript,
    #[serde(default = "default_repeat")]
    pub repeat: u32,
    #[serde(default)]
    pub sticky: bool,
}
fn default_repeat() -> u32 {
    1
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SuccessScript {
    pub response_text: Option<String>,
    pub reasoning: Option<Vec<String>>,
    /// Native blocks preserve thinking signatures, redactions and server tools.
    pub content: Option<Vec<Value>>,
    pub structured_output: Option<Value>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallTemplate>,
    pub usage: Option<TokenUsage>,
    pub stop_reason: Option<String>,
    pub finish_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub stop_details: Option<Value>,
    /// Exact count for the utility endpoint; otherwise it uses the estimate.
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub delay_before_headers_ms: u64,
    #[serde(default)]
    pub inter_event_delay_ms: u64,
    pub close_after_chunks: Option<usize>,
    #[serde(default)]
    pub malformed_sse: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCallTemplate {
    pub id: Option<String>,
    pub name: String,
    #[serde(alias = "input")]
    pub arguments: Value,
    pub raw_arguments: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioScript {
    Success(Box<SuccessScript>),
    Error {
        status: u16,
        message: String,
        error_type: String,
        code: Option<String>,
        retry_after: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        delay_before_headers_ms: u64,
    },
    Hang {
        #[serde(default)]
        delay_before_headers_ms: u64,
    },
    Transcript {
        status: u16,
        content_type: Option<String>,
        body: Option<Value>,
        /// Original JSON bytes, when the recording contains a JSON body.
        body_text: Option<String>,
        events: Option<Vec<TranscriptEvent>>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Raw {
        status: u16,
        content_type: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        chunks: Vec<RawChunk>,
        #[serde(default)]
        delay_before_headers_ms: u64,
    },
}

pub use twin_core::sse::TranscriptEvent;

impl ScenarioScript {
    pub fn script_kind(&self) -> &str {
        match self {
            Self::Success(_) => "success",
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
        if self.repeat == 0 {
            return Err("repeat must be positive".to_owned());
        }
        if !matches!(
            self.matcher.endpoint.as_str(),
            "messages" | "messages.count_tokens"
        ) {
            return Err("endpoint must be messages or messages.count_tokens".to_owned());
        }
        let (status, headers, content_type) = match &self.script {
            ScenarioScript::Success(s) => {
                if let Some(reason) = s.stop_reason.as_ref().or(s.finish_reason.as_ref()) {
                    if !matches!(
                        reason.as_str(),
                        "end_turn"
                            | "max_tokens"
                            | "stop_sequence"
                            | "tool_use"
                            | "pause_turn"
                            | "refusal"
                            | "model_context_window_exceeded"
                            | "length"
                            | "stop"
                    ) {
                        return Err(format!("unsupported stop reason: {reason}"));
                    }
                }
                (200, &s.headers, None)
            }
            ScenarioScript::Error {
                status,
                headers,
                retry_after,
                ..
            } => {
                if retry_after
                    .as_ref()
                    .is_some_and(|s| HeaderValue::try_from(s).is_err())
                {
                    return Err("invalid Retry-After header".to_owned());
                }
                (*status, headers, None)
            }
            ScenarioScript::Raw {
                status,
                headers,
                content_type,
                ..
            } => (*status, headers, content_type.as_ref()),
            ScenarioScript::Transcript {
                status,
                headers,
                content_type,
                body,
                body_text,
                events,
            } => {
                if usize::from(body.is_some())
                    + usize::from(body_text.is_some())
                    + usize::from(events.is_some())
                    != 1
                {
                    return Err(
                        "transcript requires exactly one of body, body_text, events".to_owned()
                    );
                }
                (*status, headers, content_type.as_ref())
            }
            ScenarioScript::Hang { .. } => return Ok(()),
        };
        if StatusCode::from_u16(status).is_err() || status < 200 {
            return Err("invalid final response status".to_owned());
        }
        for (key, value) in headers {
            if HeaderName::try_from(key).is_err() || HeaderValue::try_from(value).is_err() {
                return Err("invalid response header".to_owned());
            }
        }
        if content_type.is_some_and(|s| HeaderValue::try_from(s).is_err()) {
            return Err("invalid content type".to_owned());
        }
        Ok(())
    }
}

pub fn raw_outcome(
    status: u16,
    content_type: Option<String>,
    headers: BTreeMap<String, String>,
    chunks: Vec<RawChunk>,
    delay_before_headers_ms: u64,
) -> RawOutcome {
    RawOutcome {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        content_type,
        headers,
        chunks,
        delay_before_headers_ms,
    }
}
