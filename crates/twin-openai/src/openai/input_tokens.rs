use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::models::{OpenAiError, ResponsesRequest};
use crate::engine::execute_input_tokens_request;
use crate::openai::{auth, responses};
use crate::state::AppState;

/// Returns a stable approximation of the tokens in a Responses request.
///
/// A protocol twin cannot reproduce a model-specific tokenizer. Counting one
/// token per four compact JSON bytes keeps the result deterministic, includes
/// instructions and tool definitions, and grows with the complete request.
pub async fn count_input_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let namespace = match auth::openai_request_namespace(&headers, state.config.require_auth) {
        Ok(namespace) => namespace,
        Err(response) => return response,
    };

    let value = match Json::<Value>::from_bytes(&body) {
        Ok(Json(value)) => value,
        Err(rejection) => {
            return OpenAiError::from_json_rejection(&rejection)
                .into_response()
                .into_response();
        }
    };

    let request = match serde_json::from_value::<ResponsesRequest>(value.clone()) {
        Ok(request) => request,
        Err(error) => {
            return OpenAiError::invalid_request("body", &error.to_string())
                .into_response()
                .into_response();
        }
    };
    let request_hash = crate::record::request_hash(&body);
    match execute_input_tokens_request(&state, &namespace, &request, request_hash) {
        Ok(Some(outcome)) => return responses::execution_response(false, outcome).await,
        Ok(None) => {}
        Err(error) => return error.into_response().into_response(),
    }

    Json(json!({
        "object": "response.input_tokens",
        "input_tokens": approximate_input_tokens(&value)
    }))
    .into_response()
}

fn approximate_input_tokens(body: &Value) -> u64 {
    let bytes = body.to_string().len();
    let tokens = bytes.div_ceil(4).max(1);
    u64::try_from(tokens).unwrap_or(u64::MAX)
}
