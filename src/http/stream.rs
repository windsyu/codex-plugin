use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WsSubscribe {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) after_event_seq: Option<i64>,
    #[serde(default)]
    pub(super) filters: StreamFilters,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StreamFilters {
    #[serde(default)]
    pub(super) thread_keys: Vec<String>,
    #[serde(default)]
    pub(super) source_ids: Vec<String>,
    #[serde(default)]
    pub(super) methods: Vec<String>,
}

impl StreamFilters {
    pub(super) fn is_valid(&self) -> bool {
        [&self.thread_keys, &self.source_ids, &self.methods]
            .into_iter()
            .all(|values| {
                values.len() <= 100
                    && values
                        .iter()
                        .all(|value| !value.is_empty() && value.len() <= 256)
            })
    }

    pub(super) fn matches(&self, event: &Value) -> bool {
        matches_filter(&self.thread_keys, event["threadKey"].as_str())
            && matches_filter(&self.source_ids, event["sourceId"].as_str())
            && matches_filter(&self.methods, event["method"].as_str())
    }
}

fn matches_filter(values: &[String], actual: Option<&str>) -> bool {
    values.is_empty() || actual.is_some_and(|actual| values.iter().any(|value| value == actual))
}
