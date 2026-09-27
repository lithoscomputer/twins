//! TypeSafe AI's `systemone` evaluation API in twin mode.
//!
//! The endpoint shares the `/v1` root with the OpenAI surface but none of
//! its shape: it is never streamed, the model is named in the body, and the
//! canonical engine has no plan for its answers. Replay therefore serves
//! recorded scenarios only, matched by request hash, and has no
//! deterministic fallback. A recorded `x-typesafe-request-id` header is
//! replayed with the body, because clients read it back as the verdict id.
//! The request body is the request's input text, so a test can match it
//! with `input_contains` and read what the client sent in the request log.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::engine::execute_evaluation_request;
use crate::openai::auth;
use crate::openai::models::OpenAiError;
use crate::openai::responses::execution_response;
use crate::state::AppState;

/// TypeSafe's systemone route, served in twin mode and forwarded unrebased
/// in proxy-record mode.
pub const SYSTEMONE_PATH: &str = "/v1/systemone";

/// The scenario `endpoint` systemone exchanges record and replay under.
pub const SCENARIO_ENDPOINT: &str = "systemone";

/// The response header TypeSafe names each request by.
pub const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";

pub async fn create_systemone(
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
    let model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|request| {
            request
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();

    match execute_evaluation_request(
        &state,
        &namespace,
        SCENARIO_ENDPOINT,
        &model,
        String::from_utf8_lossy(&body).into_owned(),
        Some(request_hash),
    ) {
        Ok(outcome) => execution_response(false, outcome).await,
        Err(error) => error.into_response().into_response(),
    }
}
