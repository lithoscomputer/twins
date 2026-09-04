use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Clone, Debug, Serialize)]
pub struct RequestLog {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub endpoint: String,
    pub model: String,
    pub stream: bool,
    pub input_text: String,
    pub instructions_text: String,
    pub metadata: Map<String, Value>,
}
