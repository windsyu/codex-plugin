use crate::domain::model::NormalizedEvent;

pub(super) fn projection_reference_key(event: &NormalizedEvent) -> String {
    if let Some(item_id) = event.item_id.as_deref() {
        let turn_scope = event
            .turn_id
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| format!("@unassigned:{}:{}", event.source_id, event.epoch_id));
        format!("item:{}:{turn_scope}:{item_id}", event.thread_key)
    } else if let Some(turn_id) = event.turn_id.as_deref() {
        format!("turn:{}:{turn_id}", event.thread_key)
    } else {
        format!("thread:{}", event.thread_key)
    }
}

pub(super) fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
