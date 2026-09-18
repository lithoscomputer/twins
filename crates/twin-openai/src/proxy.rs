//! Proxy-record mode.
//!
//! The supported `/v1` routes are forwarded to a real upstream, with the
//! client's bearer token replaced by the configured upstream key. Successful
//! `/v1/responses` and `/v1/chat/completions` exchanges are derived into
//! scripted scenarios appended to the recording file. Model discovery and
//! input-token counts pass through without being recorded. The Vercel AI
//! Gateway's `/v4/ai/evaluation-model` and TypeSafe's `/v1/systemone` are
//! forwarded to the same upstream root and recorded as transcripts only.
//! The client's bearer token names the generation recording namespace, so
//! each test's calls replay later as an ordered per-namespace queue.
//!
//! Failed upstream responses and underivable exchanges are passed through
//! but not recorded. Admin and debug routes are not mounted in this mode.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use serde_json::{json, Map, Value};

use crate::config::{Config, RecordFormat};
use crate::engine::scenario::Scenario;
use crate::evaluation::EVALUATION_PATH;
use crate::openai::auth;
use crate::record::{
    derive_script, parse_sse_events, request_hash, ExchangeShape, RecordedEndpoint,
    RecordedExchange,
};
use crate::systemone::{REQUEST_ID_HEADER, SYSTEMONE_PATH};
use twin_core::proxy::forward_stream;
use twin_core::record::RecordingStore;

/// Upstream response headers passed back to the client and written into a
/// transcript scenario so replay serves them too. Only headers a client
/// reads back belong here: TypeSafe's request id fills the verdict id.
const REPLAYED_RESPONSE_HEADERS: [&str; 1] = [REQUEST_ID_HEADER];

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

    let mut api = Router::new()
        .route("/v1/models", get(proxy_models))
        .route("/v1/responses", post(proxy_responses))
        .route(
            "/v1/responses/input_tokens",
            post(proxy_response_input_tokens),
        )
        .route("/v1/chat/completions", post(proxy_chat))
        .route(SYSTEMONE_PATH, post(proxy_systemone))
        .route(EVALUATION_PATH, post(proxy_evaluation));
    if config.require_auth {
        api = api.layer(middleware::from_fn(auth::require_bearer_auth));
    }

    Ok(api.route("/healthz", get(healthz)).with_state(state))
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn proxy_models(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    proxy_unrecorded(state, Method::GET, "/v1/models", &headers, None).await
}

async fn proxy_response_input_tokens(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_unrecorded(
        state,
        Method::POST,
        "/v1/responses/input_tokens",
        &headers,
        Some(body),
    )
    .await
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

/// The evaluation path is not rebased: the gateway hangs it off the same
/// root as its `/v1` family, so one `upstream_url` serves both.
async fn proxy_evaluation(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_exchange(
        state,
        RecordedEndpoint::Evaluation,
        EVALUATION_PATH,
        &headers,
        body,
    )
    .await
}

/// TypeSafe hangs systemone off the same `/v1` root as chat, so the path
/// is forwarded as-is.
async fn proxy_systemone(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_exchange(
        state,
        RecordedEndpoint::SystemOne,
        SYSTEMONE_PATH,
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

    let upstream_request = build_upstream_request(&state, Method::POST, path, headers)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body);

    let upstream_response = match upstream_request.send().await {
        Ok(response) => response,
        Err(error) => return upstream_error_response(&error),
    };

    let status = upstream_response.status();
    let content_type = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned();
    let replayed_headers = replayed_headers(upstream_response.headers());
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
            replayed_headers,
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
                                &replayed_headers,
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
        passthrough_response(status, content_type, replayed_headers, Body::from(body))
    }
}

/// The allowlisted upstream response headers, keyed by lowercase name.
fn replayed_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    REPLAYED_RESPONSE_HEADERS
        .iter()
        .filter_map(|name| {
            let value = headers.get(*name)?.to_str().ok()?;
            Some(((*name).to_owned(), value.to_owned()))
        })
        .collect()
}

async fn proxy_unrecorded(
    state: ProxyState,
    method: Method,
    path: &str,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Response {
    let mut upstream_request = build_upstream_request(&state, method, path, headers);
    if let Some(body) = body {
        upstream_request = upstream_request
            .header(header::CONTENT_TYPE, "application/json")
            .body(body);
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
    let body = match upstream_response.bytes().await {
        Ok(body) => body,
        Err(error) => return upstream_error_response(&error),
    };
    passthrough_response(status, content_type, BTreeMap::new(), Body::from(body))
}

fn build_upstream_request(
    state: &ProxyState,
    method: Method,
    path: &str,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    let mut request = state
        .client
        .request(method, format!("{}{}", state.upstream_url, path))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.upstream_api_key),
        );
    // `chatgpt-account-id` and `originator` are the Codex deployment's seat
    // envelope; forwarding them costs nothing on the platform API. The
    // `ai-*` headers are the Vercel AI Gateway's protocol envelope, which
    // names the evaluation model and the specification version.
    for name in [
        "openai-organization",
        "openai-project",
        "chatgpt-account-id",
        "originator",
        "ai-gateway-protocol-version",
        "ai-evaluation-model-specification-version",
        "ai-model-id",
    ] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    request
}

#[allow(
    clippy::too_many_arguments,
    reason = "One argument per captured response fact keeps the single call site readable."
)]
fn stream_and_record(
    state: &ProxyState,
    shape: Option<ExchangeShape>,
    hash: Option<String>,
    bearer: Option<String>,
    status: StatusCode,
    content_type: Option<HeaderValue>,
    replayed_headers: BTreeMap<String, String>,
    upstream_response: reqwest::Response,
) -> Response {
    let recorder = state.recorder.clone();
    let record_format = state.record_format;
    let recorded_content_type = content_type.clone();
    let recorded_headers = replayed_headers.clone();
    let body = forward_stream(upstream_response.bytes_stream(), move |buffer| {
        if status == StatusCode::OK {
            if let Some(shape) = shape {
                match parse_sse_events(buffer) {
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
                                &recorded_headers,
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

    passthrough_response(status, content_type, replayed_headers, body)
}

fn passthrough_response(
    status: StatusCode,
    content_type: Option<HeaderValue>,
    headers: BTreeMap<String, String>,
    body: Body,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            response.headers_mut().insert(name, value);
        }
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
    store: RecordingStore,
}

impl Recorder {
    fn create(path: PathBuf, append: bool) -> Result<Self> {
        Ok(Self {
            store: RecordingStore::create::<Scenario>(path, append)?,
        })
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
        self.store
            .push_scenario(bearer, matcher, Value::Object(script));
    }

    /// Records a verbatim transcript scenario: the exchange goes into the
    /// file as its raw body or SSE events, matched by the request hash.
    /// `headers` are the allowlisted upstream response headers replay must
    /// serve again; an empty map leaves the field out.
    #[allow(
        clippy::too_many_arguments,
        reason = "One argument per captured response fact keeps the two call sites readable."
    )]
    fn record_transcript(
        &self,
        bearer: Option<&str>,
        shape: ExchangeShape,
        hash: Option<String>,
        status: StatusCode,
        content_type: Option<&HeaderValue>,
        headers: &BTreeMap<String, String>,
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
        if !headers.is_empty() {
            script.insert(
                "headers".to_owned(),
                Value::Object(
                    headers
                        .iter()
                        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                        .collect(),
                ),
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

        self.store
            .push_scenario(bearer, Value::Object(matcher), Value::Object(script));
    }
}
