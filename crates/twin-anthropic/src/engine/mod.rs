pub mod plan;
pub mod scenario;

use crate::anthropic::models::{AnthropicError, MessagesRequest};
use crate::state::{AppState, NamespaceKey};
use scenario::{RequestContext, ScenarioScript};

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
