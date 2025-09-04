use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackItem {
    pub id: String,
    pub uri: String,
    pub prepared_path: Option<String>,
    pub metadata: serde_json::Value,
}

impl TrackItem {
    #[allow(dead_code)]
    pub fn new(uri: &str, metadata: serde_json::Value) -> Self {
        Self { id: Uuid::new_v4().to_string(), uri: uri.to_string(), prepared_path: None, metadata }
    }
    pub fn new_with_prepared(uri: &str, prepared_path: Option<String>, metadata: serde_json::Value) -> Self {
        Self { id: Uuid::new_v4().to_string(), uri: uri.to_string(), prepared_path, metadata }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LoopMode {
    None,
    Track,
    Queue,
}

impl Default for LoopMode {
    fn default() -> Self {
        LoopMode::None
    }
}
