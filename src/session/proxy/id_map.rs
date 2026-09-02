use std::collections::HashMap;

use serde_json::Value;

#[derive(Debug, Default)]
pub(super) struct RequestIdMap {
    next_tui_id: u64,
    tui_requests: HashMap<String, MappedRequest>,
}

#[derive(Debug)]
pub(super) struct MappedRequest {
    pub original_id: Value,
    pub command_id: Option<String>,
}

impl RequestIdMap {
    pub fn map_tui_request(&mut self, original_id: Value, command_id: Option<String>) -> Value {
        self.next_tui_id = self.next_tui_id.saturating_add(1);
        let upstream_id = format!("gateway:tui:{}", self.next_tui_id);
        self.tui_requests.insert(
            id_key(&Value::String(upstream_id.clone())),
            MappedRequest {
                original_id,
                command_id,
            },
        );
        Value::String(upstream_id)
    }

    pub fn take_tui_response(&mut self, upstream_id: &Value) -> Option<MappedRequest> {
        self.tui_requests.remove(&id_key(upstream_id))
    }

    pub fn drain_command_ids(&mut self) -> Vec<String> {
        self.tui_requests
            .drain()
            .filter_map(|(_, mapped)| mapped.command_id)
            .collect()
    }
}

fn id_key(id: &Value) -> String {
    serde_json::to_string(id).expect("JSON-RPC id serialization cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_numeric_string_and_null_ids_in_a_separate_namespace() {
        let mut ids = RequestIdMap::default();
        for original in [json!(7), json!("7"), Value::Null] {
            let upstream = ids.map_tui_request(original.clone(), None);
            assert_ne!(upstream, original);
            assert_eq!(
                ids.take_tui_response(&upstream).unwrap().original_id,
                original
            );
        }
    }
}
