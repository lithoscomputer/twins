pub mod auth;
pub mod models;

use crate::engine::{
    plan::ResponsePlan,
    scenario::{raw_outcome, ScenarioScript, SuccessScript},
    select_script,
};
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use models::{AnthropicError, MessagesRequest};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::time::{sleep, Duration};

pub async fn messages(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    handle(&state, &headers, &body, false).await
}
pub async fn count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(&state, &headers, &body, true).await
}
pub async fn models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = auth::request_namespace(&headers, state.config.require_auth) {
        return error;
    }
    if let Err(error) = auth::validate_version(&headers) {
        return error.into_response();
    }
    Json(json!({"data":[{"id":"claude-test","type":"model","display_name":"Claude Test","created_at":"2024-01-01T00:00:00Z"}],"has_more":false,"first_id":"claude-test","last_id":"claude-test"})).into_response()
}

async fn handle(state: &AppState, headers: &HeaderMap, body: &[u8], count: bool) -> Response {
    let namespace = match auth::request_namespace(headers, state.config.require_auth) {
        Ok(ns) => ns,
        Err(r) => return r,
    };
    if let Err(error) = auth::validate_version(headers) {
        return error.into_response();
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(error) => {
            return AnthropicError::invalid_request("body", &error.to_string()).into_response()
        }
    };
    let hash = crate::record::request_hash(body);
    if let Some(script) =
        crate::engine::select_recorded_transcript(state, &namespace, &parsed, hash.clone(), count)
    {
        return transcript_response(script);
    }
    if count {
        const ALLOWED: &[&str] = &[
            "model",
            "messages",
            "system",
            "tools",
            "tool_choice",
            "thinking",
            "cache_control",
            "context_management",
        ];
        if let Some(key) = parsed
            .as_object()
            .and_then(|v| v.keys().find(|k| !ALLOWED.contains(&k.as_str())))
        {
            return AnthropicError::invalid_request(key, "not accepted by count_tokens")
                .into_response();
        }
    }
    let estimated = parsed.to_string().len().div_ceil(4) as u64;
    let request: MessagesRequest = match serde_json::from_value(parsed) {
        Ok(v) => v,
        Err(error) => {
            return AnthropicError::invalid_request("body", &error.to_string()).into_response()
        }
    };
    let script = match select_script(state, &namespace, &request, hash, count) {
        Ok(s) => s.unwrap_or(ScenarioScript::Success(Box::default())),
        Err(e) => return e.into_response(),
    };
    match script {
        ScenarioScript::Success(options) => {
            let mut response = if count {
                delay(options.delay_before_headers_ms).await;
                Json(json!({"input_tokens":options.input_tokens.unwrap_or(estimated)}))
                    .into_response()
            } else {
                let plan = match ResponsePlan::build(
                    state.next_response_id(&namespace),
                    &request,
                    &options,
                ) {
                    Ok(p) => p,
                    Err(e) => return e.into_response(),
                };
                delay(options.delay_before_headers_ms).await;
                if request.stream {
                    crate::sse::event_response(crate::sse::messages_events(&plan), &options)
                } else {
                    Json(plan.messages_json()).into_response()
                }
            };
            apply_headers(&mut response, options.headers);
            response
        }
        ScenarioScript::Error {
            status,
            message,
            error_type,
            code,
            retry_after,
            mut headers,
            delay_before_headers_ms,
        } => {
            delay(delay_before_headers_ms).await;
            let mut error = AnthropicError::new(
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                &error_type,
                message,
            );
            error.body.error.code = code;
            let mut response = error.into_response();
            if let Some(value) = retry_after {
                headers.insert("retry-after".to_owned(), value);
            }
            apply_headers(&mut response, headers);
            response
        }
        ScenarioScript::Hang {
            delay_before_headers_ms,
        } => {
            delay(delay_before_headers_ms).await;
            futures_util::future::pending::<Response>().await
        }
        ScenarioScript::Raw {
            status,
            content_type,
            headers,
            chunks,
            delay_before_headers_ms,
        } => {
            crate::transport::raw_response(raw_outcome(
                status,
                content_type,
                headers,
                chunks,
                delay_before_headers_ms,
            ))
            .await
        }
        script @ ScenarioScript::Transcript { .. } => transcript_response(script),
    }
}

fn transcript_response(script: ScenarioScript) -> Response {
    let ScenarioScript::Transcript {
        status,
        content_type,
        body,
        body_text,
        events,
        headers,
    } = script
    else {
        unreachable!("only transcript scripts use this renderer")
    };
    let mut response = if let Some(events) = events {
        crate::sse::event_response(events, &SuccessScript::default())
    } else {
        let mut r = Response::new(Body::from(
            body_text.unwrap_or_else(|| body.unwrap_or(Value::Null).to_string()),
        ));
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        r
    };
    *response.status_mut() =
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    apply_headers(&mut response, headers);
    if let Some(value) = content_type.and_then(|s| HeaderValue::try_from(s).ok()) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

pub fn apply_headers(response: &mut Response, headers: BTreeMap<String, String>) {
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            response.headers_mut().insert(name, value);
        }
    }
}
async fn delay(ms: u64) {
    if ms > 0 {
        sleep(Duration::from_millis(ms)).await;
    }
}
