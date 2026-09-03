use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::RETRY_AFTER;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::future;
use tokio::time::{sleep, Duration};

use super::models::ResponsesRequest;
use crate::engine::execute_responses_request;
use crate::engine::failures::ExecutionOutcome;
use crate::openai::auth;
use crate::sse::responses_sse_response;
use crate::state::AppState;

pub async fn create_response(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let namespace = match auth::openai_request_namespace(&headers, state.config.require_auth) {
        Ok(namespace) => namespace,
        Err(response) => return response,
    };

    // The raw bytes are parsed twice on purpose: `Json::from_bytes` keeps
    // the exact rejection semantics, and the hash of the canonicalized body
    // is what transcript scenarios match on.
    let request = match Json::<ResponsesRequest>::from_bytes(&body) {
        Ok(Json(request)) => request,
        Err(rejection) => {
            return super::models::OpenAiError::from_json_rejection(&rejection)
                .into_response()
                .into_response();
        }
    };
    let request_hash = crate::record::request_hash(&body);

    let outcome = match execute_responses_request(&state, &namespace, &request, request_hash) {
        Ok(outcome) => outcome,
        Err(error) => return error.into_response().into_response(),
    };
    execution_response(request.stream, outcome).await
}

pub(crate) async fn execution_response(stream: bool, outcome: ExecutionOutcome) -> Response {
    match outcome {
        ExecutionOutcome::Success(success) => {
            if success.transport.delay_before_headers_ms > 0 {
                sleep(Duration::from_millis(
                    success.transport.delay_before_headers_ms,
                ))
                .await;
            }

            if stream {
                responses_sse_response(&success.plan, success.transport).into_response()
            } else {
                Json(success.plan.responses_json()).into_response()
            }
        }
        ExecutionOutcome::Error(error) => {
            if error.delay_before_headers_ms > 0 {
                sleep(Duration::from_millis(error.delay_before_headers_ms)).await;
            }

            let mut response = Json(error.body).into_response();
            *response.status_mut() = error.status;
            if let Some(retry_after) = error.retry_after {
                response.headers_mut().insert(
                    RETRY_AFTER,
                    retry_after.parse().expect("valid Retry-After header"),
                );
            }
            response
        }
        ExecutionOutcome::Transcript(outcome) => {
            crate::sse::transcript_response(outcome).into_response()
        }
        ExecutionOutcome::Raw(outcome) => crate::transport::raw_response(outcome)
            .await
            .into_response(),
        ExecutionOutcome::Hang {
            delay_before_headers_ms,
        } => {
            if delay_before_headers_ms > 0 {
                sleep(Duration::from_millis(delay_before_headers_ms)).await;
            }
            future::pending::<()>().await;
            unreachable!()
        }
    }
}
