use super::*;
use crate::workbench::recording::{Recorder, RecorderOptions};

#[tokio::test]
async fn history_list_pages_do_not_repeat_runs_and_reject_changed_list_cursors() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("history");
    let hub = LiveHub::new(LiveLimits::default());
    let mut expected = std::collections::HashSet::new();
    for index in 0..21 {
        let current = if index == 0 {
            hub.clone()
        } else {
            LiveHub::new(LiveLimits::default())
        };
        expected.insert(current.epoch().to_string());
        let recorder = Recorder::start(
            &current,
            RecorderOptions::new(root.clone(), directory.path(), "Synthetic list".into()),
        )
        .unwrap();
        drop(recorder);
    }
    let server = ReadingServer::bind(hub).await.unwrap();
    let client = client();
    let cookie = cookie(&client, &server).await;
    let url = format!("{}/workbench/v1/history", server.state.origin);
    let read = |url: String| client.get(url).header(header::COOKIE, &cookie).send();
    let first: Value =
        serde_json::from_slice(&read(url.clone()).await.unwrap().bytes().await.unwrap()).unwrap();
    assert_eq!(first["runs"].as_array().unwrap().len(), 20);
    let cursor = first["nextCursor"].as_str().unwrap();
    let next_url = format!("{url}?cursor={cursor}");
    let second: Value =
        serde_json::from_slice(&read(next_url.clone()).await.unwrap().bytes().await.unwrap())
            .unwrap();
    assert_eq!(second["runs"].as_array().unwrap().len(), 1);
    assert!(second["nextCursor"].is_null());
    let actual: std::collections::HashSet<_> = first["runs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["runs"].as_array().unwrap())
        .map(|v| v["runEpoch"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(actual, expected);
    let fresh = LiveHub::new(LiveLimits::default());
    drop(
        Recorder::start(
            &fresh,
            RecorderOptions::new(root, directory.path(), "Synthetic list".into()),
        )
        .unwrap(),
    );
    assert_eq!(read(next_url).await.unwrap().status(), StatusCode::CONFLICT);
    for suffix in [
        "?before=1",
        "?unknown=1",
        "?cursor=bad",
        "/bad-epoch",
        "/00000000-0000-0000-0000-000000000000?before=-1",
    ] {
        assert_eq!(
            read(format!("{url}{suffix}")).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn history_is_paired_scoped_paginated_and_independent_of_terminal_control() {
    let directory = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let root = directory.path().join("history");
    let recorder = Recorder::start(
        &hub,
        RecorderOptions::new(root, directory.path(), "Synthetic project".into()),
    )
    .unwrap();
    publish(&hub, "historical model body");
    drop(recorder);
    let server = ReadingServer::bind(hub.clone()).await.unwrap();
    let client = client();
    let cookie = cookie(&client, &server).await;
    let list = format!("{}/workbench/v1/history", server.state.origin);
    assert_eq!(
        client.get(&list).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let response = client
        .get(&list)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(page["runs"].as_array().unwrap().len(), 1);
    let url = format!("{list}/{}", hub.epoch());
    let response = client
        .get(&url)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    let snapshot: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(snapshot["snapshot"]["items"], json!(hub.snapshot().items));
    assert_eq!(snapshot["source"], "saved_workbench");
    assert_eq!(
        client
            .get(format!("{list}?cursor=bad"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .get(format!("{list}/{}", Uuid::new_v4()))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .post(&url)
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &server.state.origin)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    let before = hub.snapshot().view_seq;
    let response = client
        .get(format!(
            "{}/workbench/v1/live/events?epoch={}&after={before}",
            server.state.origin,
            hub.epoch()
        ))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let chunk = stream.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("recorder.status"));
    assert_eq!(hub.snapshot().view_seq, before);
}
