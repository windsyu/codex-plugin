use super::*;
use crate::workbench::decode::DecoderLimits;
use crate::workbench::live::{LiveHub, LiveLimits};
use crate::workbench::observe::Observer;
use crate::workbench::redaction::RedactionPolicy;
use crate::workbench::web::ReadingServer;
use serde_json::{Value, json};

fn event(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_reading_pipeline_survives_paused_observer_bad_json_and_slow_reader() {
    let bad = "data: {private-malformed-fixture\n\n";
    let upstream = fixture(move |request| async move {
        if request.uri().path().ends_with("/pause") {
            return ([(header::CONTENT_TYPE, "text/event-stream")], vec![b'x'; 512 * 1024]).into_response();
        }
        if request.uri().path().ends_with("/bad") {
            return ([(header::CONTENT_TYPE, "text/event-stream")], bad).into_response();
        }
        let stream = async_stream::stream! {
            yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":"resp_good"}})));
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(3)).await;
                yield Ok(event(json!({"type":"response.output_text.delta","item_id":"msg_good","content_index":0,"delta":"持续可见"})));
            }
            yield Ok(event(json!({"type":"response.output_text.done","item_id":"msg_good","content_index":0,"text":"最终安全正文 fixture-secret-value"})));
            yield Ok(event(json!({"type":"response.completed","response":{"id":"resp_good"}})));
        };
        Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from_stream(stream)).unwrap()
    }).await;
    let (capture, receiver) = capture::channel(32 * 1024, 16);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    let hub = LiveHub::new(LiveLimits {
        client_events: 16,
        ..LiveLimits::default()
    });
    let _observer = Observer::start_with_delay(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec!["fixture-secret-value".into()]).unwrap(),
        DecoderLimits::default(),
        Duration::from_secs(5),
    )
    .unwrap();
    let web = ReadingServer::bind(hub.clone()).await.unwrap();
    let started = Instant::now();
    let bytes = client()
        .get(format!("{}/pause", proxy.child_base_url()))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), vec![b'x'; 512 * 1024]);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "model forwarding waited for the observer"
    );
    assert_eq!(
        hub.snapshot().view_seq,
        0,
        "the observer must still be paused"
    );
    assert!(capture.stats().retained_bytes <= 32 * 1024);
    assert!(capture.stats().dropped_chunks > 0);
    timeout(Duration::from_secs(6), async {
        while hub.snapshot().capture != "partial" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let bytes = client()
        .get(format!("{}/bad", proxy.child_base_url()))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        bytes, bad,
        "malformed JSON must still reach the native client unchanged"
    );
    let cursor = hub.snapshot().view_seq;
    let mut slow = hub.subscribe(hub.epoch(), cursor).unwrap();
    let mut fast = hub.subscribe(hub.epoch(), cursor).unwrap();
    let healthy_reader = tokio::spawn(async move {
        let mut visible = 0;
        while let Some(message) = fast.recv().await {
            assert!(!message.json.contains("private-malformed-fixture"));
            assert!(!message.json.contains("fixture-secret-value"));
            visible += usize::from(message.json.contains("持续可见"));
            if message.json.contains("最终安全正文 [已脱敏]") {
                return visible;
            }
        }
        panic!("healthy reader unexpectedly disconnected");
    });
    let bytes = client()
        .get(format!("{}/good", proxy.child_base_url()))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert!(
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .contains("fixture-secret-value"),
        "forwarded application bytes must not be redacted"
    );
    assert!(timeout(DEADLINE, healthy_reader).await.unwrap().unwrap() >= 40);
    assert!(timeout(DEADLINE, slow.recv()).await.unwrap().is_none());

    let bootstrap = web.bootstrap_url();
    let (base, token) = bootstrap.split_once("/#pair=").unwrap();
    let response = client()
        .post(format!("{base}/workbench/v1/pair"))
        .header(header::ORIGIN, base)
        .body(json!({"token":token}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let response = client()
        .get(format!("{base}/workbench/v1/live/snapshot"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    let snapshot = response.text().await.unwrap();
    assert!(snapshot.contains("invalid_json") && snapshot.contains("observation_gap"));
    assert!(snapshot.contains("最终安全正文 [已脱敏]"));
    assert!(
        !snapshot.contains("private-malformed-fixture")
            && !snapshot.contains("fixture-secret-value")
    );
    assert!(started.elapsed() >= Duration::from_secs(5));
}
