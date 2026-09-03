use axum::extract::rejection::JsonRejection;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::models::{OpenAiError, ResponsesRequest};

/// Returns a stable approximation of the tokens in a Responses request.
///
/// A protocol twin cannot reproduce a model-specific tokenizer. Counting one
/// token per four compact JSON bytes keeps the result deterministic, includes
/// instructions and tool definitions, and grows with the complete request.
pub async fn count_input_tokens(payload: Result<Json<Value>, JsonRejection>) -> Response {
    let body = match payload {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return OpenAiError::from_json_rejection(&rejection)
                .into_response()
                .into_response();
        }
    };

    let request = match serde_json::from_value::<ResponsesRequest>(body.clone()) {
        Ok(request) => request,
        Err(error) => {
            return OpenAiError::invalid_request("body", &error.to_string())
                .into_response()
                .into_response();
        }
    };
    if let Err(error) = request.validate() {
        return error.into_response().into_response();
    }

    Json(json!({
        "object": "response.input_tokens",
        "input_tokens": approximate_input_tokens(&body)
    }))
    .into_response()
}

fn approximate_input_tokens(body: &Value) -> u64 {
    let bytes = body.to_string().len();
    let tokens = bytes.div_ceil(4).max(1);
    u64::try_from(tokens).unwrap_or(u64::MAX)
}
