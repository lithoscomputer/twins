//! Proxy-record mode.
//!
//! `/v1/responses` and `/v1/chat/completions` are forwarded to a real
//! upstream (the client's bearer token is replaced with the configured
//! upstream key), the response is streamed back verbatim, and every
//! successful exchange is derived into a scripted scenario appended to the
//! recording file. The client's bearer token names the recording namespace,
//! so each test's calls replay later as an ordered per-namespace queue.
//!
//! Failed upstream responses and underivable exchanges are passed through
//! but not recorded. Admin and debug routes are not mounted in this mode.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_stream::stream;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::config::Config;
use crate::openai::auth;
use crate::record::{
    derive_script, parse_sse_events, ExchangeShape, RecordedEndpoint, RecordedExchange,
};

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    upstream_url: String,
    upstream_api_key: String,
    require_auth: bool,
    recorder: Arc<Recorder>,
}

pub fn router(config: &Config) -> Result<Router> {
    let upstream_api_key = config
        .upstream_api_key
        .clone()
        .context("proxy-record mode requires an upstream API key")?;
    let recording_path = config
        .recording_path
        .clone()
        .context("proxy-record mode requires a recording path")?;

    let state = ProxyState {
        client: reqwest::Client::builder()
            .build()
            .context("failed to build proxy HTTP client")?,
        upstream_url: config.upstream_url.clone(),
        upstream_api_key,
        require_auth: config.require_auth,
        recorder: Arc::new(Recorder::create(recording_path)?),
    };

    let mut v1 = Router::new()
        .route("/v1/responses", post(proxy_responses))
        .route("/v1/chat/completions", post(proxy_chat));
    if config.require_auth {
        v1 = v1.layer(middleware::from_fn(auth::require_bearer_auth));
    }

    Ok(v1.route("/healthz", get(healthz)).with_state(state))
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn proxy_responses(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_exchange(
        state,
        RecordedEndpoint::Responses,
        "/v1/responses",
        &headers,
        body,
    )
    .await
}

async fn proxy_chat(State(state): State<ProxyState>, headers: HeaderMap, body: Bytes) -> Response {
    proxy_exchange(
        state,
        RecordedEndpoint::ChatCompletions,
        "/v1/chat/completions",
        &headers,
        body,
    )
    .await
}

async fn proxy_exchange(
    state: ProxyState,
    endpoint: RecordedEndpoint,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let bearer = match auth::proxy_bearer_token(headers, state.require_auth) {
        Ok(bearer) => bearer,
        Err(response) => return response,
    };

    let shape = serde_json::from_slice::<Value>(&body)
        .ok()
        .map(|request| ExchangeShape::from_request(endpoint, &request));

    let mut upstream_request = state
        .client
        .post(format!("{}{}", state.upstream_url, path))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.upstream_api_key),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(body);
    for name in ["openai-organization", "openai-project"] {
        if let Some(value) = headers.get(name) {
            upstream_request = upstream_request.header(name, value);
        }
    }

    let upstream_response = match upstream_request.send().await {
        Ok(response) => response,
        Err(error) => return upstream_error_response(&error),
    };

    let status = upstream_response.status();
    let content_type = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned();
    let is_event_stream = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));

    if is_event_stream {
        stream_and_record(
            &state,
            shape,
            bearer,
            status,
            content_type,
            upstream_response,
        )
    } else {
        let body = match upstream_response.bytes().await {
            Ok(body) => body,
            Err(error) => return upstream_error_response(&error),
        };
        if status == StatusCode::OK {
            if let (Some(shape), Ok(parsed)) = (shape, serde_json::from_slice::<Value>(&body)) {
                state
                    .recorder
                    .record(bearer.as_deref(), shape, &RecordedExchange::Json(parsed));
            }
        }
        passthrough_response(status, content_type, Body::from(body))
    }
}

fn stream_and_record(
    state: &ProxyState,
    shape: Option<ExchangeShape>,
    bearer: Option<String>,
    status: StatusCode,
    content_type: Option<HeaderValue>,
    upstream_response: reqwest::Response,
) -> Response {
    let recorder = state.recorder.clone();
    let body = Body::from_stream(stream! {
        let mut buffer: Vec<u8> = Vec::new();
        let mut upstream = upstream_response.bytes_stream();
        let mut failed = false;

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(chunk) => {
                    buffer.extend_from_slice(&chunk);
                    yield Ok::<_, std::io::Error>(chunk);
                }
                Err(error) => {
                    tracing::warn!(%error, "upstream stream failed mid-response");
                    failed = true;
                    yield Err(std::io::Error::other(error));
                    break;
                }
            }
        }

        if !failed && status == StatusCode::OK {
            if let Some(shape) = shape {
                match parse_sse_events(&buffer) {
                    Ok(events) => recorder.record(
                        bearer.as_deref(),
                        shape,
                        &RecordedExchange::Stream(events),
                    ),
                    Err(error) => {
                        tracing::warn!(%error, "failed to parse streamed exchange for recording");
                    }
                }
            }
        }
    });

    passthrough_response(status, content_type, body)
}

fn passthrough_response(
    status: StatusCode,
    content_type: Option<HeaderValue>,
    body: Body,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
}

fn upstream_error_response(error: &reqwest::Error) -> Response {
    tracing::error!(%error, "failed to reach proxy upstream");
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": format!("failed to reach proxy upstream: {error}"),
                "type": "twin_proxy_error",
                "param": null,
                "code": "upstream_unreachable"
            }
        })),
    )
        .into_response()
}

struct Recorder {
    path: PathBuf,
    state: Mutex<RecorderState>,
}

#[derive(Default)]
struct RecorderState {
    scenarios: Vec<Value>,
    counters: HashMap<String, u64>,
}

impl Recorder {
    /// Creates the recorder and truncates the recording file, so a server
    /// run always produces a complete, self-consistent recording.
    fn create(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create recording directory {}", parent.display())
            })?;
        }

        let recorder = Self {
            path,
            state: Mutex::new(RecorderState::default()),
        };
        recorder
            .flush(&RecorderState::default())
            .context("failed to initialize recording file")?;
        Ok(recorder)
    }

    fn record(&self, bearer: Option<&str>, shape: ExchangeShape, exchange: &RecordedExchange) {
        let script = match derive_script(shape, exchange) {
            Ok(script) => script,
            Err(error) => {
                tracing::warn!(%error, "skipping underivable exchange in proxy recording");
                return;
            }
        };

        let mut state = self.state.lock().expect("recorder lock");
        let namespace_label = bearer.unwrap_or("global");
        let sequence = {
            let counter = state
                .counters
                .entry(namespace_label.to_owned())
                .or_insert(0);
            *counter += 1;
            *counter
        };

        let mut scenario = Map::new();
        scenario.insert(
            "scenario_id".to_owned(),
            Value::String(format!("{namespace_label}/{sequence:04}")),
        );
        if let Some(bearer) = bearer {
            scenario.insert("namespace".to_owned(), Value::String(bearer.to_owned()));
        }
        scenario.insert(
            "matcher".to_owned(),
            json!({
                "endpoint": shape.endpoint.scenario_endpoint(),
                "stream": shape.stream,
            }),
        );
        scenario.insert("script".to_owned(), Value::Object(script));
        state.scenarios.push(Value::Object(scenario));

        if let Err(error) = self.flush(&state) {
            tracing::error!(%error, "failed to write proxy recording");
        }
    }

    fn flush(&self, state: &RecorderState) -> Result<()> {
        let mut contents = serde_json::to_string_pretty(&json!({ "scenarios": state.scenarios }))
            .context("failed to serialize recording")?;
        contents.push('\n');
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, contents)
            .with_context(|| format!("failed to write recording to {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to move recording into {}", self.path.display()))?;
        Ok(())
    }
}
