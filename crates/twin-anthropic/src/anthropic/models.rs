use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Debug, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub system: Value,
    #[serde(default)]
    pub tools: Vec<Value>,
    #[serde(default)]
    pub tool_choice: Value,
    #[serde(default)]
    pub thinking: Value,
    #[serde(default)]
    pub output_config: Value,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorBody {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AnthropicError {
    pub status: StatusCode,
    pub body: ErrorEnvelope,
}

impl AnthropicError {
    pub fn new(status: StatusCode, error_type: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorEnvelope {
                kind: "error".to_owned(),
                error: ErrorBody {
                    error_type: error_type.to_owned(),
                    message: message.into(),
                    code: None,
                },
            },
        }
    }

    pub fn invalid_request(param: &str, message: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("{param}: {message}"),
        )
    }

    pub fn scenario_not_found() -> Self {
        let mut error = Self::invalid_request("scenario", "no matching scenario remains");
        error.body.error.code = Some("scenario_not_found".to_owned());
        error
    }
}

impl IntoResponse for AnthropicError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl MessagesRequest {
    pub fn validate(&self, count_tokens: bool) -> Result<(), AnthropicError> {
        let invalid = AnthropicError::invalid_request;
        if self.model.trim().is_empty() {
            return Err(invalid("model", "must not be empty"));
        }
        if !count_tokens && self.max_tokens.is_none_or(|n| n == 0) {
            return Err(invalid("max_tokens", "must be a positive integer"));
        }
        if self.messages.is_empty() {
            return Err(invalid("messages", "must not be empty"));
        }
        for message in &self.messages {
            if !matches!(message.role.as_str(), "user" | "assistant" | "system") {
                return Err(invalid(
                    "messages.role",
                    "must be user, assistant, or system",
                ));
            }
            validate_content(&message.content, false)?;
        }
        if !self.system.is_null() {
            validate_content(&self.system, true)?;
        }
        for tool in &self.tools {
            require_text(tool, "name")?;
            if tool.get("type").is_none_or(|t| t == "custom")
                && !tool.get("input_schema").is_some_and(Value::is_object)
            {
                return Err(invalid("tools.input_schema", "must be an object"));
            }
        }
        if !self.tool_choice.is_null() {
            let kind = self.tool_choice.get("type").and_then(Value::as_str);
            if !matches!(kind, Some("auto" | "any" | "tool" | "none")) {
                return Err(invalid(
                    "tool_choice.type",
                    "must be auto, any, tool, or none",
                ));
            }
            if matches!(kind, Some("any" | "tool")) && self.tools.is_empty() {
                return Err(invalid("tools", "required for forced tool use"));
            }
            if kind == Some("tool") {
                let name = require_text(&self.tool_choice, "name")?;
                if !self.tools.iter().any(|t| t["name"] == name) {
                    return Err(invalid("tool_choice.name", "must name a supplied tool"));
                }
            }
            if matches!(kind, Some("any" | "tool")) && self.thinking_enabled() {
                return Err(invalid(
                    "thinking",
                    "cannot be combined with forced tool use",
                ));
            }
        }
        if !self.thinking.is_null() {
            match self.thinking.get("type").and_then(Value::as_str) {
                Some("enabled") => {
                    let budget = self.thinking.get("budget_tokens").and_then(Value::as_u64);
                    if (!count_tokens && budget.is_none())
                        || budget.is_some_and(|n| {
                            n < 1024
                                || (!count_tokens && self.max_tokens.is_some_and(|max| n >= max))
                        })
                    {
                        return Err(invalid(
                            "thinking.budget_tokens",
                            "must be at least 1024 and below max_tokens",
                        ));
                    }
                }
                Some("adaptive" | "disabled") => {}
                _ => {
                    return Err(invalid(
                        "thinking.type",
                        "must be enabled, adaptive, or disabled",
                    ))
                }
            }
        }
        if let Some(format) = self.output_config.get("format") {
            if format["type"] != "json_schema" {
                return Err(invalid(
                    "output_config.format.type",
                    "only json_schema is supported",
                ));
            }
            validate_schema(&format["schema"], true)?;
        }
        Ok(())
    }

    pub fn extract_user_text(&self) -> String {
        normalize(
            &self
                .messages
                .iter()
                .filter(|m| m.role == "user")
                .map(|m| content_text(&m.content))
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    pub fn extract_instruction_text(&self) -> String {
        let mut parts = vec![content_text(&self.system)];
        parts.extend(
            self.messages
                .iter()
                .filter(|m| m.role == "system")
                .map(|m| content_text(&m.content)),
        );
        normalize(&parts.join(" "))
    }

    pub fn thinking_enabled(&self) -> bool {
        matches!(self.thinking["type"].as_str(), Some("enabled" | "adaptive"))
    }

    pub fn schema(&self) -> Option<&Value> {
        self.output_config.get("format")?.get("schema")
    }
}

pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().map(content_text).collect::<Vec<_>>().join(" "),
        Value::Object(part) => match part.get("type").and_then(Value::as_str) {
            Some("text") => part
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            Some("tool_result" | "system") => {
                part.get("content").map(content_text).unwrap_or_default()
            }
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn require_text<'a>(value: &'a Value, field: &str) -> Result<&'a str, AnthropicError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AnthropicError::invalid_request(field, "must be a non-empty string"))
}

fn validate_content(content: &Value, system: bool) -> Result<(), AnthropicError> {
    if let Some(text) = content.as_str() {
        return if text.is_empty() {
            Err(AnthropicError::invalid_request(
                "content",
                "must not be empty",
            ))
        } else {
            Ok(())
        };
    }
    let parts = content
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            AnthropicError::invalid_request("content", "must be a string or non-empty array")
        })?;
    for part in parts {
        let kind = require_text(part, "type")?;
        if system && kind != "text" {
            return Err(AnthropicError::invalid_request(
                "system",
                "only text blocks are supported",
            ));
        }
        match kind {
            "text" => {
                require_text(part, "text")?;
            }
            "image" | "document" => {
                let source = &part["source"];
                match source["type"].as_str() {
                    Some("base64") => {
                        require_text(source, "data")?;
                        require_text(source, "media_type")?;
                    }
                    Some("url") => {
                        require_text(source, "url")?;
                    }
                    Some("text") if kind == "document" => {
                        require_text(source, "data")?;
                    }
                    Some("content") if kind == "document" => {
                        validate_content(&source["content"], false)?;
                    }
                    _ => {
                        return Err(AnthropicError::invalid_request(
                            "source",
                            "unsupported image or document source",
                        ))
                    }
                }
            }
            "tool_use" => {
                require_text(part, "id")?;
                require_text(part, "name")?;
                if !part["input"].is_object() {
                    return Err(AnthropicError::invalid_request(
                        "input",
                        "tool input must be an object",
                    ));
                }
            }
            "tool_result" => {
                require_text(part, "tool_use_id")?;
                if let Some(value) = part.get("content") {
                    if value != "" && value != &json!([]) {
                        validate_content(value, false)?;
                    }
                }
            }
            "thinking" => {
                require_text(part, "signature")?;
                if !part["thinking"].is_string() {
                    return Err(AnthropicError::invalid_request("thinking", "must be text"));
                }
            }
            "redacted_thinking" => {
                require_text(part, "data")?;
            }
            "system" => {
                validate_content(&part["content"], true)?;
            }
            "audio" => {
                return Err(AnthropicError::invalid_request(
                    "content",
                    "audio is not supported",
                ))
            }
            // Native server-tool blocks and future extensions are retained.
            _ => {}
        }
    }
    Ok(())
}

pub fn validate_schema(schema: &Value, root: bool) -> Result<(), AnthropicError> {
    let invalid = || {
        AnthropicError::invalid_request("schema", "supported subset: object roots, nested objects, string, integer, number, boolean, enum, const")
    };
    if !schema.is_object()
        || schema.get("anyOf").is_some()
        || schema.get("oneOf").is_some()
        || schema.get("$ref").is_some()
    {
        return Err(invalid());
    }
    let kind = schema["type"].as_str();
    if root && kind != Some("object") {
        return Err(invalid());
    }
    match kind {
        Some("object") => {
            if let Some(properties) = schema.get("properties") {
                let properties = properties.as_object().ok_or_else(invalid)?;
                for child in properties.values() {
                    validate_schema(child, false)?;
                }
            }
        }
        Some("string" | "integer" | "number" | "boolean") => {}
        _ if !root
            && (schema.get("const").is_some()
                || schema
                    .get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|v| !v.is_empty())) => {}
        _ => return Err(invalid()),
    }
    Ok(())
}

pub fn generate_json(schema: &Value, text: &str) -> Value {
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
    {
        return value.clone();
    }
    match schema["type"].as_str() {
        Some("object") => Value::Object(
            schema["properties"]
                .as_object()
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(k, v)| (k.clone(), generate_json(v, text)))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Some("integer") => json!(1),
        Some("number") => json!(1.0),
        Some("boolean") => json!(true),
        _ => json!(text),
    }
}
