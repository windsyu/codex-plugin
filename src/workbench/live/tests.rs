use super::*;
use crate::workbench::redaction::RedactionPolicy;

#[tokio::test]
async fn run_usage_snapshot_and_recording_replay_keep_the_same_totals_and_gaps() {
    use crate::workbench::decode::details::usage;
    use serde_json::{Value, json};
    let hub = LiveHub::new(LiveLimits::default());
    let mut checkpoint = hub.recording_checkpoint();
    let mut reader = hub.subscribe(hub.epoch(), 0).unwrap();
    let request = Uuid::new_v4();
    for change in [
        Change::Response {
            response_id: Some("r".into()),
            status: ResponseStatus::Receiving,
            model: None,
            usage: None,
        },
        Change::Response {
            response_id: Some("r".into()),
            status: ResponseStatus::Completed,
            model: None,
            usage: usage(
                &json!({"input_tokens":10,"output_tokens":3,"input_tokens_details":{"cached_tokens":6},"output_tokens_details":{"reasoning_tokens":2}}),
            ),
        },
        Change::Diagnostic {
            code: DiagnosticCode::ObservationGap,
        },
    ] {
        hub.apply(Decoded {
            request_id: request,
            capture_seq: 1,
            received_at: Instant::now(),
            change,
        });
    }
    for _ in 0..hub.snapshot().view_seq {
        let event: Value = serde_json::from_str(&reader.recv().await.unwrap().json).unwrap();
        checkpoint.event(&event).unwrap();
    }
    let snapshot = serde_json::to_value(hub.snapshot()).unwrap();
    assert_eq!(
        checkpoint.snapshot["usageSummary"],
        snapshot["usageSummary"]
    );
    assert_eq!(snapshot["usageSummary"]["totalTokens"]["tokens"], 13);
    assert_eq!(snapshot["usageSummary"]["captureIncomplete"], true);
    checkpoint
        .snapshot
        .as_object_mut()
        .unwrap()
        .remove("usageSummary");
    assert!(
        checkpoint.validate(hub.epoch()),
        "old saved views remain readable"
    );
    assert_eq!(
        LiveHub::new(LiveLimits::default())
            .snapshot()
            .usage_summary
            .response_count,
        0
    );
}

#[test]
fn run_usage_survives_response_eviction_and_late_duplicate_or_conflicting_reports() {
    use crate::workbench::decode::details::usage;
    use serde_json::json;
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    let emit = |index: usize, tokens| {
        hub.apply(Decoded {
            request_id: id,
            capture_seq: 1,
            received_at: Instant::now(),
            change: Change::Response {
                response_id: Some(format!("response-{index}")),
                status: ResponseStatus::Completed,
                model: None,
                usage: usage(&json!({"input_tokens": tokens, "output_tokens": 2, "input_tokens_details": {"cached_tokens": 4}, "output_tokens_details": {"reasoning_tokens": 1}})),
            },
        });
    };
    for index in 0..300 {
        emit(index, 10);
    }
    assert_eq!(hub.snapshot().responses.len(), 256);
    let snapshot = serde_json::to_value(hub.snapshot()).unwrap();
    assert_eq!(snapshot["usageSummary"]["totalTokens"]["tokens"], 3600);
    emit(0, 10);
    assert_eq!(
        serde_json::to_value(hub.snapshot()).unwrap()["usageSummary"],
        snapshot["usageSummary"]
    );
    emit(0, 11);
    let current = serde_json::to_value(hub.snapshot()).unwrap();
    assert_eq!(current["usageSummary"]["responseCount"], 300);
    assert_eq!(current["usageSummary"]["excludedResponses"], 1);
    assert_eq!(current["usageSummary"]["totalTokens"]["tokens"], 3588);
    emit(1, 12);
    let current = hub.snapshot();
    let late = current
        .responses
        .iter()
        .find(|response| response.response_id.as_deref() == Some("response-1"))
        .unwrap();
    assert!(
        late.usage_conflict,
        "a conflict after preview eviction must remain visible in response details"
    );
    assert_eq!(late.usage.as_ref().unwrap().input_tokens, Some(10));
}

fn event(id: Uuid, name: &str, text: &str, replace: bool) -> Decoded {
    let key = TextKey {
        request_id: id,
        response_id: Some("resp_fixture".into()),
        wire_item_id: name.into(),
        content_index: 0,
    };
    let text = RedactionPolicy::new(vec![]).unwrap().scrub(text);
    Decoded {
        request_id: id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: if replace {
            Change::TextReplace { key, text }
        } else {
            Change::TextDelta { key, text }
        },
    }
}

#[test]
fn response_model_evidence_survives_completion_and_conflicts_stay_visible_per_response() {
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    let emit = |response: &str, model: Option<&str>, status| {
        hub.apply(Decoded {
            request_id: id,
            capture_seq: 1,
            received_at: Instant::now(),
            change: Change::Response {
                response_id: Some(response.into()),
                status,
                model: model.map(|model| policy.scrub(model)),
                usage: None,
            },
        })
    };
    emit("resp_a", Some("reported_a"), ResponseStatus::Receiving);
    emit("resp_b", Some("reported_b"), ResponseStatus::Receiving);
    emit("resp_a", None, ResponseStatus::Completed);
    assert_eq!(hub.snapshot().responses[0].reported_models, ["reported_a"]);
    assert_eq!(hub.snapshot().responses[1].reported_models, ["reported_b"]);
    emit("resp_a", Some("conflict"), ResponseStatus::Completed);
    emit("resp_a", Some("third-report"), ResponseStatus::Completed);
    let snapshot = hub.snapshot();
    assert_eq!(
        snapshot.responses[0].reported_models,
        ["reported_a", "conflict"]
    );
    assert_eq!(snapshot.capture, "partial");
}

#[tokio::test]
async fn snapshot_then_subscription_has_exact_replay_live_order_and_final_replacement() {
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    hub.apply(event(id, "msg", "中", false));
    let snapshot = hub.snapshot();
    hub.apply(event(id, "msg", "间态", false));
    let mut reader = hub
        .subscribe(snapshot.run_epoch, snapshot.view_seq)
        .unwrap();
    hub.apply(event(id, "msg", "最终正文", true));
    let replay = reader.recv().await.unwrap();
    let live = reader.recv().await.unwrap();
    assert_eq!(replay.sequence, snapshot.view_seq + 1);
    assert_eq!(live.sequence, replay.sequence + 1);
    let replay: serde_json::Value = serde_json::from_str(&replay.json).unwrap();
    assert_eq!(replay["kind"], "item.patch");
    assert_eq!(replay["baseRevision"], 1);
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.model_items()[0].text, "最终正文");
    assert_eq!(snapshot.model_items()[0].revision, 3);
    assert_eq!(snapshot.recorder, "disabled");
    assert_eq!(snapshot.persisted_through_view_seq, 0);
    hub.apply(event(id, "msg", "最终正文", true));
    assert_eq!(hub.snapshot().view_seq, snapshot.view_seq);
}

#[test]
fn response_usage_and_observed_duration_remain_per_response_and_repeats_do_not_erase_them() {
    use crate::workbench::decode::details::usage;
    use serde_json::json;
    use std::time::Duration;
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    let start = Instant::now();
    let emit = |response: &str, millis, status, count| {
        hub.apply(Decoded {
            request_id: id,
            capture_seq: millis + 1,
            received_at: start + Duration::from_millis(millis),
            change: Change::Response {
                response_id: Some(response.into()),
                status,
                model: None,
                usage: count,
            },
        });
    };
    let first = usage(&json!({"input_tokens":20,"output_tokens":4,"total_tokens":24}));
    let second = usage(&json!({"input_tokens":40,"output_tokens":8,"total_tokens":48}));
    emit("a", 0, ResponseStatus::Receiving, None);
    emit("b", 10, ResponseStatus::Receiving, None);
    emit("b", 30, ResponseStatus::Completed, second.clone());
    emit("a", 80, ResponseStatus::Completed, first.clone());
    let view = hub.snapshot();
    assert_eq!(view.responses[0].usage, first);
    assert_eq!(view.responses[0].observed_duration_ms, Some(80.0));
    assert_eq!(view.responses[1].usage, second);
    assert_eq!(view.responses[1].observed_duration_ms, Some(20.0));
    emit("a", 100, ResponseStatus::Completed, None);
    assert_eq!(hub.snapshot().view_seq, view.view_seq);
    emit("a", 110, ResponseStatus::Completed, second);
    let view = hub.snapshot();
    assert!(view.responses[0].usage_conflict && !view.responses[1].usage_conflict);
    assert_eq!(view.responses[0].usage, first);
    assert_eq!(view.responses[0].observed_duration_ms, Some(80.0));
    assert_eq!(view.capture, "partial");
    emit("no-start", 120, ResponseStatus::Completed, None);
    assert!(hub.snapshot().responses[2].observed_duration_ms.is_none());
}

#[tokio::test]
async fn private_context_is_read_on_demand_and_never_published_in_snapshot_or_live_ring() {
    use crate::workbench::decode::details;
    use serde_json::json;
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    let mut subscription = hub.subscribe(hub.epoch(), 0).unwrap();
    hub.apply(Decoded {
        request_id: id,
        capture_seq: 7,
        received_at: Instant::now(),
        change: Change::Document {
            document: details::request(
                &json!({"instructions":"on-demand-context-only"}),
                None,
                &RedactionPolicy::new(vec![]).unwrap(),
            ),
        },
    });
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.view_seq, 0);
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("on-demand-context-only")
    );
    let page = hub.request_details(id, None).unwrap();
    assert!(page.request_captured && page.entries.iter().all(|entry| entry.capture_seq == 7));
    assert!(
        serde_json::to_string(&page)
            .unwrap()
            .contains("on-demand-context-only")
    );
    hub.apply(event(id, "message", "public", false));
    let pushed = subscription.recv().await.unwrap();
    assert_eq!(pushed.sequence, 1);
    assert!(!pushed.json.contains("on-demand-context-only"));
}

#[tokio::test]
async fn slow_subscriber_is_disconnected_without_delaying_other_readers() {
    let hub = LiveHub::new(LiveLimits {
        client_events: 1,
        ..LiveLimits::default()
    });
    let id = Uuid::new_v4();
    let mut slow = hub.subscribe(hub.epoch(), 0).unwrap();
    let mut fast = hub.subscribe(hub.epoch(), 0).unwrap();
    hub.apply(event(id, "msg", "one ", false));
    assert_eq!(fast.recv().await.unwrap().sequence, 1);
    hub.apply(event(id, "msg", "two ", false));
    assert_eq!(fast.recv().await.unwrap().sequence, 2);
    assert!(slow.recv().await.is_none());
    assert_eq!(hub.snapshot().model_items()[0].text, "one two ");
}

#[tokio::test]
async fn client_byte_budget_and_stale_cursor_require_a_new_snapshot() {
    let hub = LiveHub::new(LiveLimits {
        client_bytes: 128,
        ring_bytes: 256,
        ..LiveLimits::default()
    });
    let id = Uuid::new_v4();
    let mut reader = hub.subscribe(hub.epoch(), 0).unwrap();
    hub.apply(event(id, "msg", &"中".repeat(200), false));
    assert!(reader.recv().await.is_none());
    assert!(matches!(
        hub.subscribe(hub.epoch(), 0),
        Err(SubscribeError::SnapshotRequired)
    ));
    assert!(matches!(
        hub.subscribe(Uuid::new_v4(), 1),
        Err(SubscribeError::SnapshotRequired)
    ));
    assert!(matches!(
        hub.subscribe(hub.epoch(), 999),
        Err(SubscribeError::SnapshotRequired)
    ));
}

#[tokio::test]
async fn item_limits_are_utf8_safe_and_eviction_invalidates_old_reading_state() {
    let hub = LiveHub::new(LiveLimits {
        item_bytes: 5,
        view_bytes: 5,
        items: 1,
        ..LiveLimits::default()
    });
    let id = Uuid::new_v4();
    hub.apply(event(id, "first", "中文截断", false));
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.model_items()[0].text, "中");
    assert!(snapshot.model_items()[0].truncated);
    let mut reader = hub.subscribe(hub.epoch(), snapshot.view_seq).unwrap();
    hub.apply(event(id, "second", "new", false));
    assert!(reader.recv().await.is_none());
    assert!(matches!(
        hub.subscribe(hub.epoch(), snapshot.view_seq),
        Err(SubscribeError::SnapshotRequired)
    ));
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.model_items().len(), 1);
    assert_eq!(snapshot.model_items()[0].key.wire_item_id, "second");
    assert_eq!(snapshot.capture, "partial");
}

#[test]
fn out_of_band_loss_counters_cannot_be_hidden_by_losing_the_gap_event() {
    let hub = LiveHub::new(LiveLimits::default());
    let stats = CaptureStats {
        retained_bytes: 0,
        byte_limit: 8,
        dropped_bytes: 40,
        dropped_chunks: 4,
        interrupted_streams: 0,
        transports: Default::default(),
    };
    hub.capture_health(stats);
    assert_eq!(hub.snapshot().capture, "partial");
    assert_eq!(
        hub.snapshot().diagnostics[0].code,
        DiagnosticCode::ObservationGap
    );
    let seq = hub.snapshot().view_seq;
    hub.capture_health(stats);
    assert_eq!(hub.snapshot().view_seq, seq);
}

mod view_contract;
