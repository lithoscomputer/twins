pub mod plan;
pub mod scenario;

use crate::anthropic::models::{AnthropicError, MessagesRequest};
use crate::state::{AppState, NamespaceKey};
use scenario::{RequestContext, ScenarioScript};
use serde_json::Value;

/// Exact recorded exchanges do not need the deterministic generator's schema.
pub(crate) fn select_recorded_transcript(
    state: &AppState,
    namespace: &NamespaceKey,
    request: &Value,
    hash: Option<String>,
    count: bool,
) -> Option<ScenarioScript> {
    use crate::anthropic::models::{content_text, normalize};
    let turns = request["messages"].as_array().cloned().unwrap_or_default();
    let text_for = |role| {
        turns
            .iter()
            .filter(|m| m["role"] == role)
            .map(|m| content_text(&m["content"]))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let context = RequestContext {
        endpoint: if count {
            "messages.count_tokens"
        } else {
            "messages"
        }
        .to_owned(),
        model: request["model"].as_str().unwrap_or_default().to_owned(),
        stream: request["stream"] == true,
        metadata: request["metadata"].as_object().cloned().unwrap_or_default(),
        input_text: normalize(&text_for("user")),
        instructions_text: normalize(&format!(
            "{} {}",
            content_text(&request["system"]),
            text_for("system")
        )),
        request_hash: hash,
    };
    let scenario = state.take_matching_scenario_if(namespace, &context, |s| {
        s.matcher.request_hash.is_some() && matches!(s.script, ScenarioScript::Transcript { .. })
    })?;
    state.log_request(namespace, context, scenario.scenario_id);
    Some(scenario.script)
}

pub fn select_script(
    state: &AppState,
    namespace: &NamespaceKey,
    request: &MessagesRequest,
    hash: Option<String>,
    count: bool,
) -> Result<Option<ScenarioScript>, AnthropicError> {
    request.validate(count)?;
    let context = RequestContext {
        endpoint: if count {
            "messages.count_tokens"
        } else {
            "messages"
        }
        .to_owned(),
        model: request.model.clone(),
        stream: request.stream,
        metadata: request.metadata.clone(),
        input_text: request.extract_user_text(),
        instructions_text: request.extract_instruction_text(),
        request_hash: hash,
    };
    let scenario = state.take_matching_scenario(namespace, &context);
    let unmatched = !count
        && scenario.is_none()
        && state.config.scenarios_path.is_some()
        && !state.config.allow_unmatched;
    if !count || scenario.is_some() {
        state.log_request(
            namespace,
            context,
            scenario.as_ref().and_then(|s| s.scenario_id.clone()),
        );
    }
    if unmatched {
        return Err(AnthropicError::scenario_not_found());
    }
    Ok(scenario.map(|s| s.script))
}
