use super::scenario::SuccessScript;
use crate::anthropic::models::{generate_json, AnthropicError, MessagesRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl Default for TokenUsage {
    fn default() -> Self {
        Self {
            input_tokens: 1,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            extra: Map::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResponsePlan {
    pub id: String,
    pub model: String,
    pub content: Vec<Value>,
    pub usage: TokenUsage,
    pub stop_reason: String,
    pub stop_sequence: Option<String>,
    pub stop_details: Option<Value>,
    pub raw_arguments: BTreeMap<usize, String>,
}

impl ResponsePlan {
    pub fn build(
        number: u64,
        request: &MessagesRequest,
        script: &SuccessScript,
    ) -> Result<Self, AnthropicError> {
        let mut content = Vec::new();
        let mut raw_arguments = BTreeMap::new();
        let text = script
            .response_text
            .clone()
            .unwrap_or_else(|| format!("deterministic: {}", request.extract_user_text()));
        let reasoning = script.reasoning.clone().unwrap_or_else(|| {
            if request.thinking_enabled() {
                vec![format!("reasoning: {}", request.extract_user_text())]
            } else {
                Vec::new()
            }
        });
        for (index, thought) in reasoning.iter().enumerate() {
            content.push(json!({"type":"thinking","thinking":thought,"signature":format!("twin-signature-{number}-{index}")}));
        }
        if let Some(blocks) = &script.content {
            // A native content script owns the entire block sequence.
            content.clone_from(blocks);
        } else {
            let structured = script
                .structured_output
                .clone()
                .or_else(|| request.schema().map(|schema| generate_json(schema, &text)));
            if let Some(value) = structured {
                content.push(json!({"type":"text","text":value.to_string()}));
            } else if script.response_text.is_some() || script.tool_calls.is_empty() {
                content.push(json!({"type":"text","text":text}));
            }
            for (index, tool) in script.tool_calls.iter().enumerate() {
                if let Some(raw) = &tool.raw_arguments {
                    raw_arguments.insert(content.len(), raw.clone());
                }
                content.push(json!({"type":"tool_use", "id":tool.id.clone().unwrap_or_else(|| format!("toolu_{number}_{index}")), "name":tool.name,"input":tool.arguments}));
            }
        }
        let tools: Vec<_> = content.iter().filter(|b| b["type"] == "tool_use").collect();
        let invalid = |message| AnthropicError::invalid_request("tool_choice", message);
        match request.tool_choice["type"].as_str() {
            Some("none") if !tools.is_empty() => {
                return Err(invalid("forbids tool calls in the response plan"))
            }
            Some("any") if tools.is_empty() => {
                return Err(invalid("requires a scripted tool call"))
            }
            Some("tool")
                if tools.is_empty()
                    || tools
                        .iter()
                        .any(|t| t["name"] != request.tool_choice["name"]) =>
            {
                return Err(invalid("requires the named scripted tool"))
            }
            _ => {}
        }
        let stop_reason = script
            .stop_reason
            .clone()
            .or_else(|| {
                script.finish_reason.as_ref().map(|s| match s.as_str() {
                    "length" => "max_tokens".to_owned(),
                    "stop" => {
                        if tools.is_empty() {
                            "end_turn".to_owned()
                        } else {
                            "tool_use".to_owned()
                        }
                    }
                    _ => s.clone(),
                })
            })
            .unwrap_or_else(|| {
                if tools.is_empty() {
                    "end_turn".to_owned()
                } else {
                    "tool_use".to_owned()
                }
            });
        Ok(Self {
            id: format!("msg_{number:06}"),
            model: request.model.clone(),
            content,
            usage: script.usage.clone().unwrap_or_default(),
            stop_reason,
            stop_sequence: script.stop_sequence.clone(),
            stop_details: script.stop_details.clone(),
            raw_arguments,
        })
    }

    pub fn messages_json(&self) -> Value {
        let mut body = json!({"id":self.id,"type":"message","role":"assistant","model":self.model,"content":self.content,"stop_reason":self.stop_reason,"stop_sequence":self.stop_sequence,"usage":self.usage});
        if let Some(details) = &self.stop_details {
            body["stop_details"] = details.clone();
        }
        body
    }
}
