use anyhow::Result;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::config::Mode;
use crate::state::AppState;
use crate::{admin, debug_ui, openai, proxy};

pub fn router(state: AppState) -> Result<Router> {
    if state.config.mode == Mode::ProxyRecord {
        return proxy::router(&state.config);
    }

    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", openai::router(state.config.require_auth));

    if state.config.enable_admin {
        router = router.merge(admin::router()).merge(debug_ui::router());
    }

    Ok(router.with_state(state))
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}
