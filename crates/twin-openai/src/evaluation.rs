//! The Vercel AI Gateway's evaluation-model API in twin mode.
//!
//! The endpoint sits outside the OpenAI surface: it is never streamed, the
//! model is named by the `ai-model-id` header rather than the body, and the
//! canonical engine has no plan for its answers. Replay therefore serves
//! recorded scenarios only, matched by request hash, and has no
//! deterministic fallback.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use crate::engine::execute_evaluation_request;
use crate::openai::auth;
use crate::openai::models::OpenAiError;
use crate::openai::responses::execution_response;
use crate::state::AppState;

/// The gateway's evaluation-model route, served in twin mode and forwarded
/// unrebased in proxy-record mode.
pub const EVALUATION_PATH: &str = "/v4/ai/evaluation-model";

/// The header the gateway names the evaluation model in.
pub const MODEL_ID_HEADER: &str = "ai-model-id";

pub async fn create_evaluation(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let namespace = match auth::openai_request_namespace(&headers, state.config.require_auth) {
        Ok(namespace) => namespace,
        Err(response) => return response,
    };

    let Some(request_hash) = crate::record::request_hash(&body) else {
        return OpenAiError::invalid_request("body", "request body must be JSON")
            .into_response()
            .into_response();
    };
    let model = headers
        .get(MODEL_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    match execute_evaluation_request(&state, &namespace, model, Some(request_hash)) {
        Ok(outcome) => execution_response(false, outcome).await,
        Err(error) => error.into_response().into_response(),
    }
}
