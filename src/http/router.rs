use serde_json::{Value, json};

use crate::config::Config;

pub(super) fn settings_snapshot(config: &Config) -> Value {
    json!({
        "server":{"bind":config.server.bind.to_string(),"transport":"loopback_http","strictOrigin":config.server.strict_origin},
        "capture":{"maxRawEventBytes":config.capture.max_raw_event_bytes,"inlineBlobBytes":config.capture.inline_blob_bytes,
          "keepReasoning":config.capture.keep_reasoning,"keepRawJson":config.capture.keep_raw_json,
          "ingestQueueEvents":config.capture.ingest_queue_events,"apiConsumerQueueEvents":config.capture.api_consumer_queue_events},
        "storage":{"rawEventRetentionDays":config.storage.raw_event_retention_days,"deltaRetentionDays":config.storage.delta_retention_days,
          "blobRetentionDays":config.storage.blob_retention_days},
        "sources":config.sources.iter().map(|source| json!({"name":source.name,"liveMode":source.live_mode,
          "scanIntervalSeconds":source.scan_interval_seconds})).collect::<Vec<_>>()
    })
}
