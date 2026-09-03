use axum::Json;
use serde_json::{json, Value};

/// Lists the deterministic model identity accepted by the twin.
///
/// Generation routes remain permissive and accept any non-empty model ID.
/// This representative entry gives clients that require discovery a stable,
/// OpenAI-shaped model to select in black-box tests.
pub async fn list_models() -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": [{
            "id": "gpt-test",
            "object": "model",
            "created": 0,
            "owned_by": "twin-openai",
            "shutdown_date": null
        }]
    }))
}
