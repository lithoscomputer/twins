use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};

use crate::state::AppState;
use twin_core::debug_ui::render_html;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/__debug", get(debug_page))
        .route("/__debug/state.json", get(debug_state_json))
}

async fn debug_page(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.debug_snapshot();
    Html(render_html(&snapshot, "twin-anthropic"))
}

async fn debug_state_json(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.debug_snapshot())
}
