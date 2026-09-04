use crate::{admin, anthropic, config::Mode, debug_ui, proxy, state::AppState};
use anyhow::Result;
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

pub fn router(state: AppState) -> Result<Router> {
    if state.config.mode == Mode::ProxyRecord {
        return proxy::router(&state.config);
    }
    let mut router = Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/models", get(anthropic::models))
        .route("/v1/messages", post(anthropic::messages))
        .route("/v1/messages/count_tokens", post(anthropic::count_tokens));
    if state.config.enable_admin {
        router = router.merge(admin::router()).merge(debug_ui::router());
    }
    Ok(router
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state))
}
