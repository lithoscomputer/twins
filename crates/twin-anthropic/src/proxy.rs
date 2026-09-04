//! Forward native Anthropic traffic and record successful Messages exchanges.
use crate::anthropic::{apply_headers, auth, models::AnthropicError};
use crate::config::{Config, RecordFormat};
use crate::engine::scenario::{validate_scenario_ids, ScenarioEnvelope};
use crate::record::{derive_script, message_from_events, parse_sse_events, request_hash};
use anyhow::{Context, Result};
use async_stream::stream;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    config: Config,
    recorder: Arc<Recorder>,
}

pub fn router(config: &Config) -> Result<Router> {
    let state = ProxyState {
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        config: config.clone(),
        recorder: Arc::new(Recorder::create(
            config
                .recording_path
                .clone()
                .context("missing recording path")?,
            config.recording_append,
        )?),
    };
    Ok(Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/models", get(models))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state))
}
async fn messages(State(state): State<ProxyState>, headers: HeaderMap, body: Bytes) -> Response {
    let path = state
        .config
        .upstream_messages_path
        .clone()
        .unwrap_or_else(|| "/v1/messages".to_owned());
    forward(state, Method::POST, &path, headers, Some(body), true).await
}
async fn count_tokens(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(
        state,
        Method::POST,
        "/v1/messages/count_tokens",
        headers,
        Some(body),
        false,
    )
    .await
}
async fn models(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    forward(state, Method::GET, "/v1/models", headers, None, false).await
}

async fn forward(
    state: ProxyState,
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: Option<Bytes>,
    record: bool,
) -> Response {
    let namespace = match auth::api_key(&headers, state.config.require_auth) {
        Ok(key) => key,
        Err(r) => return r,
    };
    if let Err(e) = auth::validate_version(&headers) {
        return e.into_response();
    }
    let request_body = body.as_deref().unwrap_or_default();
    let hash = request_hash(request_body);
    let requested_stream = serde_json::from_slice::<Value>(request_body)
        .ok()
        .is_some_and(|v| v["stream"] == true);
    let mut request = state
        .client
        .request(method, format!("{}{}", state.config.upstream_url, path))
        .header(
            "x-api-key",
            state.config.upstream_api_key.as_deref().unwrap_or_default(),
        );
    for name in [
        "anthropic-version",
        "anthropic-beta",
        "accept",
        "user-agent",
    ] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    if let Some(body) = body {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let upstream = match request.send().await {
        Ok(r) => r,
        Err(e) => return upstream_error(&e),
    };
    let status = upstream.status();
    let response_headers = retained_headers(upstream.headers());
    let is_stream = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map_or(requested_stream, |s| s.contains("text/event-stream"));
    if is_stream {
        let response_headers_copy = response_headers.clone();
        let body = Body::from_stream(stream! {
            let mut buffer = Vec::new();
            let mut upstream = upstream.bytes_stream();
            let mut failed = false;
            while let Some(chunk) = upstream.next().await {
                match chunk {
                    Ok(chunk) => { buffer.extend_from_slice(&chunk); yield Ok::<_,std::io::Error>(chunk); }
                    Err(error) => { failed = true; yield Err(std::io::Error::other(error)); break; }
                }
            }
            if record && !failed && status == StatusCode::OK {
                state.recorder.record(namespace.as_deref(),hash.as_deref(),requested_stream,true,state.config.record_format,status,&response_headers_copy,&buffer);
            }
        });
        passthrough(status, response_headers, body)
    } else {
        let body = match upstream.bytes().await {
            Ok(b) => b,
            Err(e) => return upstream_error(&e),
        };
        if record && status == StatusCode::OK {
            state.recorder.record(
                namespace.as_deref(),
                hash.as_deref(),
                requested_stream,
                false,
                state.config.record_format,
                status,
                &response_headers,
                &body,
            );
        }
        passthrough(status, response_headers, Body::from(body))
    }
}

fn retained_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "connection"
                    | "transfer-encoding"
                    | "content-length"
                    | "keep-alive"
                    | "proxy-authenticate"
                    | "proxy-authorization"
                    | "te"
                    | "trailer"
                    | "upgrade"
                    | "authorization"
                    | "x-api-key"
            )
        })
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_owned())))
        .collect()
}
fn passthrough(status: StatusCode, headers: BTreeMap<String, String>, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    apply_headers(&mut response, headers);
    response
}
fn upstream_error(error: &reqwest::Error) -> Response {
    tracing::error!(%error,"failed to reach proxy upstream");
    AnthropicError::new(
        StatusCode::BAD_GATEWAY,
        "api_error",
        "failed to reach proxy upstream",
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
    fn create(path: PathBuf, append: bool) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
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
        recorder.flush(&recorder.state.lock().expect("recorder lock"))?;
        Ok(recorder)
    }

    fn record(
        &self,
        namespace: Option<&str>,
        hash: Option<&str>,
        requested_stream: bool,
        is_stream: bool,
        format: RecordFormat,
        status: StatusCode,
        headers: &BTreeMap<String, String>,
        bytes: &[u8],
    ) {
        let script = (|| -> Result<Value> {
            let events = if is_stream {
                Some(parse_sse_events(bytes)?)
            } else {
                None
            };
            match format {
                RecordFormat::Semantic => {
                    let message = if let Some(events) = events {
                        message_from_events(&events)?
                    } else {
                        serde_json::from_slice(bytes)?
                    };
                    derive_script(&message)
                }
                RecordFormat::Transcript => {
                    let mut script =
                        json!({"kind":"transcript","status":status.as_u16(),"headers":headers});
                    if let Some(events) = events {
                        script["events"] = serde_json::to_value(events)?;
                    } else {
                        let _: Value = serde_json::from_slice(bytes)?;
                        script["body_text"] = json!(std::str::from_utf8(bytes)?);
                    }
                    Ok(script)
                }
            }
        })();
        let script = match script {
            Ok(script) => script,
            Err(error) => {
                tracing::warn!(%error,"passing through underivable exchange without recording");
                return;
            }
        };
        let mut matcher = json!({"endpoint":"messages","stream":requested_stream});
        if format == RecordFormat::Transcript {
            matcher["request_hash"] = json!(hash);
        }
        let mut state = self.state.lock().expect("recorder lock");
        let label = namespace.unwrap_or("global");
        let counter = state.counters.entry(label.to_owned()).or_default();
        *counter += 1;
        let id = format!("{label}/{counter:04}");
        let mut scenario = json!({"scenario_id":id,"matcher":matcher,"script":script});
        if let Some(namespace) = namespace {
            scenario["namespace"] = json!(namespace);
        }
        state.scenarios.push(scenario);
        if let Err(error) = self.flush(&state) {
            tracing::error!(%error,"failed to write recording");
        }
    }

    fn flush(&self, state: &RecorderState) -> Result<()> {
        let mut data = serde_json::to_vec_pretty(&json!({"scenarios":state.scenarios}))?;
        data.push(b'\n');
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, data).with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to replace {}", self.path.display()))
    }
}
fn load_recorder_state(path: &Path) -> Result<RecorderState> {
    let bytes = fs::read(path)?;
    let envelope: ScenarioEnvelope = serde_json::from_slice(&bytes)?;
    validate_scenario_ids(&envelope.scenarios).map_err(anyhow::Error::msg)?;
    let document: Value = serde_json::from_slice(&bytes)?;
    let scenarios = document["scenarios"]
        .as_array()
        .context("missing scenarios")?
        .clone();
    let mut counters: HashMap<String, u64> = HashMap::new();
    for scenario in &scenarios {
        if let Some((label, n)) = scenario["scenario_id"]
            .as_str()
            .and_then(|s| s.rsplit_once('/'))
        {
            if let Ok(n) = n.parse::<u64>() {
                let counter = counters.entry(label.to_owned()).or_default();
                *counter = (*counter).max(n);
            }
        }
    }
    Ok(RecorderState {
        scenarios,
        counters,
    })
}
