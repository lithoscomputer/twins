use serde_json::{json, Value};
use twin_anthropic::engine::scenario::TranscriptEvent;

/// Contract projection: generated values are volatile; block and event shapes
/// and stop reasons remain part of the contract.
pub fn canonical(message: &Value, events: Option<&[TranscriptEvent]>) -> Value {
    let content:Vec<Value>=message["content"].as_array().expect("message content").iter().map(|block| {
        match block["type"].as_str() {
            Some("text")=>json!({"type":"text","text":block["text"].is_string()}),
            Some("thinking")=>json!({"type":"thinking","thinking":block["thinking"].is_string(),"signature":block["signature"].is_string()}),
            Some("tool_use")=>json!({"type":"tool_use","id":block["id"].is_string(),"name":block["name"],"input":block["input"].is_object()}),
            Some("redacted_thinking")=>json!({"type":"redacted_thinking","data":block["data"].is_string()}),
            _=>json!({"type":block["type"]}),
        }
    }).collect();
    let mut result = json!({"type":message["type"],"role":message["role"],"id":message["id"].is_string(),"model":message["model"].is_string(),"content":content,"stop_reason":message["stop_reason"],"stop_sequence":message["stop_sequence"],"usage":{"input_tokens":message["usage"]["input_tokens"].is_u64(),"output_tokens":message["usage"]["output_tokens"].is_u64()}});
    if let Some(events) = events {
        let mut sequence = Vec::new();
        for event in events {
            let data: Value = serde_json::from_str(&event.data).expect("event JSON");
            let name = data["type"].as_str().unwrap_or_default();
            if name == "ping" {
                continue;
            }
            let label = if name == "content_block_delta" {
                format!(
                    "{name}:{}",
                    data["delta"]["type"].as_str().unwrap_or_default()
                )
            } else {
                name.to_owned()
            };
            if name != "content_block_delta" || sequence.last() != Some(&label) {
                sequence.push(label);
            }
        }
        result["events"] = json!(sequence);
    }
    result
}

pub fn extras(message: &Value) -> Value {
    let supported = [
        "id",
        "type",
        "role",
        "model",
        "content",
        "stop_reason",
        "stop_sequence",
        "stop_details",
        "usage",
    ];
    let mut keys: Vec<_> = message
        .as_object()
        .expect("message")
        .keys()
        .filter(|k| !supported.contains(&k.as_str()))
        .cloned()
        .collect();
    keys.sort();
    json!(keys)
}
