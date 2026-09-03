use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl TokenUsage {
    #[must_use]
    pub const fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    #[must_use]
    pub const fn total_tokens(self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    #[must_use]
    pub fn responses_json(self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "total_tokens": self.total_tokens(),
        })
    }

    #[must_use]
    pub fn chat_completions_json(self) -> Value {
        json!({
            "prompt_tokens": self.input_tokens,
            "completion_tokens": self.output_tokens,
            "total_tokens": self.total_tokens(),
        })
    }
}

impl Default for TokenUsage {
    fn default() -> Self {
        Self::new(1, 5)
    }
}

#[derive(Clone, Debug)]
pub struct ResponsePlan {
    pub id: String,
    pub created: u64,
    pub model: String,
    pub response_text: String,
    pub structured_output: Option<Value>,
    pub reasoning: Vec<String>,
    pub tool_calls: Vec<ToolCallPlan>,
    pub usage: TokenUsage,
    /// The output token limit cut this response off.
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct ToolCallPlan {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub raw_arguments: Option<String>,
    /// A free-form `custom_tool_call` rather than a JSON `function_call`.
    pub custom: bool,
}

impl ResponsePlan {
    /// The payload text for a tool call: the raw arguments when the script
    /// gave them, the JSON string itself for a custom call whose arguments
    /// are already text, and the serialized JSON otherwise.
    pub fn tool_call_arguments_text(tool_call: &ToolCallPlan) -> String {
        if let Some(raw) = &tool_call.raw_arguments {
            return raw.clone();
        }
        match &tool_call.arguments {
            Value::String(text) if tool_call.custom => text.clone(),
            other => other.to_string(),
        }
    }

    pub fn responses_tool_call_item(tool_call: &ToolCallPlan) -> Value {
        if tool_call.custom {
            return json!({
                "id": format!("ctc_{}", tool_call.id),
                "type": "custom_tool_call",
                "call_id": tool_call.id,
                "name": tool_call.name,
                "input": Self::tool_call_arguments_text(tool_call),
            });
        }
        json!({
            "id": format!("fc_{}", tool_call.id),
            "type": "function_call",
            "call_id": tool_call.id,
            "name": tool_call.name,
            "arguments": Self::tool_call_arguments_text(tool_call),
        })
    }

    /// The `finish_reason` a Chat Completions choice reports for this plan.
    pub fn chat_finish_reason(&self) -> &'static str {
        if self.truncated {
            "length"
        } else if self.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        }
    }

    /// The `status` a Responses document reports for this plan.
    pub fn responses_status(&self) -> &'static str {
        if self.truncated {
            "incomplete"
        } else {
            "completed"
        }
    }

    pub fn chat_content(&self) -> String {
        self.structured_output
            .as_ref()
            .map_or_else(|| self.response_text.clone(), ToString::to_string)
    }

    pub fn responses_json(&self) -> Value {
        let mut content_items = Vec::new();

        if !self.response_text.is_empty() {
            content_items.push(json!({
                "type": "output_text",
                "text": self.response_text,
            }));
        }

        if let Some(structured_output) = &self.structured_output {
            content_items.push(json!({
                "type": "output_json",
                "json": structured_output,
            }));
        }

        let mut output = Vec::new();

        if !content_items.is_empty() {
            output.push(json!({
                "id": format!("msg_{}", self.id),
                "type": "message",
                "role": "assistant",
                "content": content_items,
            }));
        }

        for tool_call in &self.tool_calls {
            output.push(Self::responses_tool_call_item(tool_call));
        }

        let mut response = json!({
            "id": self.id,
            "object": "response",
            "created": self.created,
            "model": self.model,
            "status": self.responses_status(),
            "reasoning": self.reasoning,
            "output": output,
            "usage": self.usage.responses_json()
        });
        if self.truncated {
            response["incomplete_details"] = json!({ "reason": "max_output_tokens" });
        }
        response
    }

    pub fn chat_completions_json(&self) -> Value {
        json!({
            "id": format!("chatcmpl_{}", self.id),
            "object": "chat.completion",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "finish_reason": self.chat_finish_reason(),
                "message": {
                    "role": "assistant",
                    "content": self.chat_content(),
                    "reasoning": self.reasoning,
                    "tool_calls": self.tool_calls.iter().map(|tool_call| json!({
                        "id": tool_call.id,
                        "type": "function",
                        "function": {
                            "name": tool_call.name,
                            "arguments": Self::tool_call_arguments_text(tool_call),
                        }
                    })).collect::<Vec<_>>(),
                }
            }],
            "usage": self.usage.chat_completions_json()
        })
    }
}
