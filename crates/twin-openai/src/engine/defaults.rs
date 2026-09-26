use serde_json::{json, Value};

use super::plan::{ResponsePlan, TokenUsage};
use crate::openai::models::{normalize_whitespace, schema_kinds, ResponseFormat, ResponsesRequest};

pub fn build_default_response_plan(
    response_number: u64,
    request: &ResponsesRequest,
) -> ResponsePlan {
    let normalized_text = request.extract_user_text();
    let response_text = format!("deterministic: {normalized_text}");
    let input_tokens = normalized_text.split_whitespace().count() as u64;

    let structured_output = request.response_format().and_then(|format| match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({
            "message": response_text,
            "model": request.model,
        })),
        ResponseFormat::JsonSchema(schema) => {
            Some(generate_json_from_schema(&schema, &response_text))
        }
    });
    let reasoning = if request.reasoning.is_some() {
        vec![format!("reasoning: {normalized_text}")]
    } else {
        Vec::new()
    };

    ResponsePlan {
        id: format!("resp_{response_number:06}"),
        created: response_number,
        model: request.model.clone(),
        response_text,
        structured_output,
        reasoning,
        tool_calls: Vec::new(),
        usage: TokenUsage::new(input_tokens, 5),
        truncated: false,
    }
}

pub fn build_default_chat_plan(
    response_number: u64,
    model: String,
    input_text: &str,
    response_format: Option<ResponseFormat>,
    reasoning_requested: bool,
) -> ResponsePlan {
    let normalized_text = normalize_whitespace(input_text);
    let response_text = format!("deterministic: {normalized_text}");
    let structured_output = response_format.and_then(|format| match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({
            "message": response_text,
            "model": model,
        })),
        ResponseFormat::JsonSchema(schema) => {
            Some(generate_json_from_schema(&schema, &response_text))
        }
    });
    let reasoning = if reasoning_requested {
        vec![format!("reasoning: {normalized_text}")]
    } else {
        Vec::new()
    };
    let input_tokens = normalized_text.split_whitespace().count() as u64;

    ResponsePlan {
        id: format!("resp_{response_number:06}"),
        created: response_number,
        model,
        response_text,
        structured_output,
        reasoning,
        tool_calls: Vec::new(),
        usage: TokenUsage::new(input_tokens, 5),
        truncated: false,
    }
}

fn generate_json_from_schema(schema: &Value, response_text: &str) -> Value {
    let schema = schema.get("schema").unwrap_or(schema);

    match schema.get("type").and_then(Value::as_str) {
        Some("object") => object_from_properties(schema, response_text),
        _ => json!({ "message": response_text }),
    }
}

fn object_from_properties(schema: &Value, response_text: &str) -> Value {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut object = serde_json::Map::new();
    for (name, property_schema) in properties {
        object.insert(
            name,
            primitive_value_for_schema(&property_schema, response_text),
        );
    }
    Value::Object(object)
}

fn primitive_value_for_schema(schema: &Value, response_text: &str) -> Value {
    // A nullable value takes its non-null form, so fallbacks show the
    // populated shape.
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        return branches
            .iter()
            .find(|branch| !is_null_schema(branch))
            .map_or(Value::Null, |branch| {
                primitive_value_for_schema(branch, response_text)
            });
    }
    let kind = schema_kinds(schema)
        .unwrap_or_default()
        .into_iter()
        .find(|kind| *kind != "null");
    match kind {
        Some("string") => Value::String(response_text.to_owned()),
        Some("integer") => json!(1),
        Some("number") => json!(1.0),
        Some("boolean") => json!(true),
        Some("array") => json!([]),
        Some("object") => object_from_properties(schema, response_text),
        _ => Value::Null,
    }
}

fn is_null_schema(schema: &Value) -> bool {
    schema_kinds(schema).is_some_and(|kinds| kinds == ["null"])
}
