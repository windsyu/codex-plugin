#[tokio::test]
async fn replacing_stopped_run_closes_old_events_without_stopping_application_or_new_run() {
    use futures_util::StreamExt;

    let fixture = Fixture::new();
    let app = fixture.app(Overrides {
        codex_bin: Some(fixture.cli()),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let request = Connect {
        project: Some(fixture.cwd.clone()),
        ..Default::default()
    };
    let first = connect(&app.entry, request.clone()).await.unwrap().unwrap();
    let response = get(
        &app,
        &cookie,
        &format!(
            "/workbench/v1/runs/{}/live/events?epoch={}&after=0",
            first.run_id, first.run_id
        ),
    )
    .await;
    assert_eq!(response.status(), 200);
    let mut events = response.bytes_stream();
    // Receiving the initial recording status proves the old stream is polled
    // and retaining its Run state before stopping/replacing that Run.
    let initial = tokio::time::timeout(Duration::from_secs(3), events.next())
        .await
        .expect("old Run SSE must start")
        .expect("old Run SSE must be open")
        .unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("recorder.status"));

    let stop = launch_post(
        &app,
        &cookie,
        &format!("/workbench/v1/runs/{}/stop", first.run_id),
        json!({"epoch":first.run_id}),
    )
    .await;
    assert!(stop.status().is_success());
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let body = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
            if body["runs"][0]["state"] == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first Run must stop");

    let second = connect(&app.entry, request).await.unwrap().unwrap();
    assert_ne!(first.run_id, second.run_id);
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(chunk) = events.next().await {
            chunk.expect("retired Run SSE must close cleanly");
        }
    })
    .await
    .expect("replacing a Run must close its existing SSE without Application shutdown");

    assert!(app.healthy());
    let application = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
    assert_eq!(application["runs"][0]["runId"], second.run_id.to_string());
    assert_eq!(application["runs"][0]["state"], "running");
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{}/live/snapshot", second.run_id)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{}/live/snapshot", first.run_id)
        )
        .await
        .status(),
        404
    );
    assert_eq!(unsafe { libc::kill(second.cli_pid as i32, 0) }, 0);
    drop(app);
    assert_eq!(unsafe { libc::kill(second.cli_pid as i32, 0) }, -1);
}
