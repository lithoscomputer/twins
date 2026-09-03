use axum::http::StatusCode;
use serde_json::Value;

use super::plan::ResponsePlan;
use super::scenario::TranscriptEvent;
use crate::openai::models::{ErrorBody, ErrorEnvelope};
use crate::transport::RawOutcome;

#[derive(Clone, Copy, Debug, Default)]
pub struct TransportOptions {
    pub delay_before_headers_ms: u64,
    pub inter_event_delay_ms: u64,
    pub close_after_chunks: Option<usize>,
    pub malformed_sse: bool,
}

#[derive(Clone, Debug)]
pub struct SuccessOutcome {
    pub plan: ResponsePlan,
    pub transport: TransportOptions,
}

#[derive(Clone, Debug)]
pub struct ErrorOutcome {
    pub status: StatusCode,
    pub body: ErrorEnvelope,
    pub retry_after: Option<String>,
    pub delay_before_headers_ms: u64,
}

#[derive(Clone, Debug)]
pub enum ExecutionOutcome {
    Success(SuccessOutcome),
    Error(ErrorOutcome),
    Hang {
        delay_before_headers_ms: u64,
    },
    /// A verbatim recorded exchange to send back exactly as captured.
    Transcript(TranscriptOutcome),
    /// An exact sequence of response body chunks or a body failure.
    Raw(RawOutcome),
}

/// A recorded exchange replayed without the canonical engine.
#[derive(Clone, Debug)]
pub struct TranscriptOutcome {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: TranscriptBody,
}

#[derive(Clone, Debug)]
pub enum TranscriptBody {
    /// A non-streaming JSON body, verbatim.
    Json(Value),
    /// An SSE body as its ordered recorded events, one chunk per event.
    Events(Vec<TranscriptEvent>),
}

impl ErrorOutcome {
    pub fn new(
        status: StatusCode,
        message: String,
        error_type: String,
        code: String,
        retry_after: Option<String>,
        delay_before_headers_ms: u64,
    ) -> Self {
        Self {
            status,
            body: ErrorEnvelope {
                error: ErrorBody {
                    message,
                    error_type,
                    param: serde_json::Value::Null,
                    code,
                },
            },
            retry_after,
            delay_before_headers_ms,
        }
    }
}
