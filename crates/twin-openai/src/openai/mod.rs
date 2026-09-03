pub mod auth;
pub mod chat_completions;
pub mod input_tokens;
pub mod model_catalog;
pub mod models;
pub mod responses;

use axum::routing::{get, post};
use axum::{middleware, Router};

use crate::state::AppState;

pub fn router(require_auth: bool) -> Router<AppState> {
    let router = Router::new()
        .route("/models", get(model_catalog::list_models))
        .route("/responses", post(responses::create_response))
        .route(
            "/responses/input_tokens",
            post(input_tokens::count_input_tokens),
        )
        .route(
            "/chat/completions",
            post(chat_completions::create_chat_completion),
        );

    if require_auth {
        router.layer(middleware::from_fn(auth::require_bearer_auth))
    } else {
        router
    }
}
