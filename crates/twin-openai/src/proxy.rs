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
use std::path::{Path, PathBuf};
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

use crate::config::{Config, RecordFormat};
use crate::openai::auth;
use crate::record::{
    derive_script, parse_sse_events, request_hash, ExchangeShape, RecordedEndpoint,
    RecordedExchange,
};

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    upstream_url: String,
    /// Where `/v1/responses` traffic lands on the upstream. OpenAI's Codex
    /// deployment serves the unversioned `<base>/responses`, so the path is
    /// rebased rather than echoed.
    upstream_responses_path: String,
    upstream_api_key: String,
    require_auth: bool,
    record_format: RecordFormat,
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
        upstream_responses_path: config
            .upstream_responses_path
            .clone()
            .unwrap_or_else(|| "/v1/responses".to_owned()),
        upstream_api_key,
        require_auth: config.require_auth,
        record_format: config.record_format,
        recorder: Arc::new(Recorder::create(recording_path, config.recording_append)?),
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
    let path = state.upstream_responses_path.clone();
    proxy_exchange(state, RecordedEndpoint::Responses, &path, &headers, body).await
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
    let hash = request_hash(&body);

    let mut upstream_request = state
        .client
        .post(format!("{}{}", state.upstream_url, path))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.upstream_api_key),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(body);
    // `chatgpt-account-id` and `originator` are the Codex deployment's seat
    // envelope; forwarding them costs nothing on the platform API.
    for name in [
        "openai-organization",
        "openai-project",
        "chatgpt-account-id",
        "originator",
    ] {
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
    // The OpenAI Codex deployment answers a streaming request with no
    // content-type header at all, so a missing header falls back to the
    // request's own stream flag rather than the JSON path, which would
    // silently fail to parse SSE bytes and record nothing.
    let is_event_stream = match content_type.as_ref().and_then(|value| value.to_str().ok()) {
        Some(value) => value.contains("text/event-stream"),
        None => shape.is_some_and(|shape| shape.stream),
    };

    if is_event_stream {
        stream_and_record(
            &state,
            shape,
            hash,
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
            match (shape, serde_json::from_slice::<Value>(&body)) {
                (Some(shape), Ok(parsed)) => {
                    let exchange = RecordedExchange::Json(parsed);
                    match state.record_format {
                        RecordFormat::Semantic => {
                            state.recorder.record(bearer.as_deref(), shape, &exchange);
                        }
                        RecordFormat::Transcript => {
                            state.recorder.record_transcript(
                                bearer.as_deref(),
                                shape,
                                hash,
                                status,
                                content_type.as_ref(),
                                &exchange,
                            );
                        }
                    }
                }
                (None, _) => {
                    tracing::warn!("passing through an OK exchange whose request was not JSON; nothing recorded");
                }
                (_, Err(error)) => {
                    tracing::warn!(%error, "passing through an OK non-JSON body; nothing recorded");
                }
            }
        }
        passthrough_response(status, content_type, Body::from(body))
    }
}

fn stream_and_record(
    state: &ProxyState,
    shape: Option<ExchangeShape>,
    hash: Option<String>,
    bearer: Option<String>,
    status: StatusCode,
    content_type: Option<HeaderValue>,
    upstream_response: reqwest::Response,
) -> Response {
    let recorder = state.recorder.clone();
    let record_format = state.record_format;
    let recorded_content_type = content_type.clone();
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
                    Ok(events) => {
                        let exchange = RecordedExchange::Stream(events);
                        match record_format {
                            RecordFormat::Semantic => {
                                recorder.record(bearer.as_deref(), shape, &exchange);
                            }
                            RecordFormat::Transcript => recorder.record_transcript(
                                bearer.as_deref(),
                                shape,
                                hash,
                                status,
                                recorded_content_type.as_ref(),
                                &exchange,
                            ),
                        }
                    }
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
    /// Creates the recorder. By default the recording file is truncated, so
    /// a server run produces a complete, self-consistent recording; with
    /// `append` an existing file's scenarios are kept and new exchanges
    /// continue each namespace's numbering after them.
    fn create(path: PathBuf, append: bool) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create recording directory {}", parent.display())
            })?;
        }

        let state = if append && path.exists() {
            load_recorder_state(&path)?
        } else {
            RecorderState::default()
        };

        let recorder = Self {
            path,
            state: Mutex::new(state),
        };
        {
            let state = recorder.state.lock().expect("recorder lock");
            recorder
                .flush(&state)
                .context("failed to initialize recording file")?;
        }
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
        let matcher = json!({
            "endpoint": shape.endpoint.scenario_endpoint(),
            "stream": shape.stream,
        });
        self.push_scenario(bearer, matcher, Value::Object(script));
    }

    /// Records a verbatim transcript scenario: the exchange goes into the
    /// file as its raw body or SSE events, matched by the request hash.
    fn record_transcript(
        &self,
        bearer: Option<&str>,
        shape: ExchangeShape,
        hash: Option<String>,
        status: StatusCode,
        content_type: Option<&HeaderValue>,
        exchange: &RecordedExchange,
    ) {
        let mut matcher = Map::new();
        matcher.insert(
            "endpoint".to_owned(),
            Value::String(shape.endpoint.scenario_endpoint().to_owned()),
        );
        matcher.insert("stream".to_owned(), Value::Bool(shape.stream));
        if let Some(hash) = hash {
            matcher.insert("request_hash".to_owned(), Value::String(hash));
        }

        let mut script = Map::new();
        script.insert("kind".to_owned(), Value::String("transcript".to_owned()));
        script.insert("status".to_owned(), Value::from(status.as_u16()));
        if let Some(content_type) = content_type.and_then(|value| value.to_str().ok()) {
            script.insert(
                "content_type".to_owned(),
                Value::String(content_type.to_owned()),
            );
        }
        match exchange {
            RecordedExchange::Json(body) => {
                script.insert("body".to_owned(), body.clone());
            }
            RecordedExchange::Stream(events) => {
                let events: Vec<Value> = events
                    .iter()
                    .map(|event| match &event.event {
                        Some(name) => json!({ "event": name, "data": event.data }),
                        None => json!({ "data": event.data }),
                    })
                    .collect();
                script.insert("events".to_owned(), Value::Array(events));
            }
        }

        self.push_scenario(bearer, Value::Object(matcher), Value::Object(script));
    }

    fn push_scenario(&self, bearer: Option<&str>, matcher: Value, script: Value) {
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
        scenario.insert("matcher".to_owned(), matcher);
        scenario.insert("script".to_owned(), script);
        state.scenarios.push(Value::Object(scenario));
        tracing::info!(
            scenario_id = format!("{namespace_label}/{sequence:04}"),
            "recorded proxy exchange"
        );

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

/// Loads an existing recording so an append run continues it: the scenarios
/// are kept, and each namespace's counter resumes after the highest
/// recorded sequence number.
fn load_recorder_state(path: &Path) -> Result<RecorderState> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read existing recording {}", path.display()))?;
    let document: Value = serde_json::from_str(&contents)
        .with_context(|| format!("existing recording {} is not valid JSON", path.display()))?;
    let scenarios = document
        .get("scenarios")
        .and_then(Value::as_array)
        .cloned()
        .with_context(|| {
            format!(
                "existing recording {} has no scenarios array",
                path.display()
            )
        })?;

    let mut counters: HashMap<String, u64> = HashMap::new();
    for scenario in &scenarios {
        let Some((label, sequence)) = scenario
            .get("scenario_id")
            .and_then(Value::as_str)
            .and_then(|id| id.rsplit_once('/'))
        else {
            continue;
        };
        let Ok(sequence) = sequence.parse::<u64>() else {
            continue;
        };
        let counter = counters.entry(label.to_owned()).or_insert(0);
        *counter = (*counter).max(sequence);
    }

    Ok(RecorderState {
        scenarios,
        counters,
    })
}
