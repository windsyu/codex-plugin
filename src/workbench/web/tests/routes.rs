use super::*;
use tower::ServiceExt;

#[tokio::test]
async fn standalone_document_bootstraps_scoped_pair_and_run_without_widening_access() {
    let server = ReadingServer::bind(LiveHub::new(LiveLimits::default()))
        .await
        .unwrap();
    let client = client();
    let origin = &server.state.origin;
    let epoch = server.state.hub.epoch();
    let base = format!("{origin}/workbench/v1/runs/{epoch}");
    let document = client
        .get(format!("{origin}/runs/{epoch}/chat"))
        .send()
        .await
        .unwrap();
    assert_eq!(document.status(), StatusCode::OK);
    let pair = client
        .post(format!("{base}/pair"))
        .header(header::ORIGIN, origin)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"token":server.state.pairing}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(
        pair.status(),
        StatusCode::NO_CONTENT,
        "canonical bootstrap must pair through its Run scope"
    );
    let cookie = pair.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    for tail in ["run", "live/snapshot"] {
        assert_eq!(
            client
                .get(format!("{base}/{tail}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let response = client
            .get(format!("{base}/{tail}"))
            .header(header::COOKIE, cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{tail}");
        assert_eq!(
            serde_json::from_slice::<Value>(&response.bytes().await.unwrap()).unwrap()["runEpoch"],
            epoch.to_string()
        );
    }
    for (name, value) in [
        (header::HOST.as_str(), "evil.invalid"),
        (header::ORIGIN.as_str(), "https://evil.invalid"),
        ("sec-fetch-site", "cross-site"),
    ] {
        assert_eq!(
            client
                .get(format!("{base}/run"))
                .header(header::COOKIE, cookie)
                .header(name, value)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let absolute = router(server.state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("{base}/run"))
                .header(header::HOST, &server.state.authority)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        absolute.status(),
        StatusCode::FORBIDDEN,
        "rewrite must not erase the absolute-form boundary check"
    );
    assert_eq!(
        client
            .post(format!("{base}/pair"))
            .body(json!({"token":server.state.pairing}).to_string())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for (body, status) in [
        ("invalid-json".to_owned(), StatusCode::BAD_REQUEST),
        ("x".repeat(1025), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        assert_eq!(
            client
                .post(format!("{base}/pair"))
                .header(header::ORIGIN, origin)
                .body(body)
                .send()
                .await
                .unwrap()
                .status(),
            status
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/run"))
            .header(header::COOKIE, cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    // Preserve both Query and Path extractors when forwarding scoped requests.
    let request_id = Uuid::new_v4();
    let details = client
        .get(format!("{base}/requests/{request_id}?epoch={epoch}"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(details.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        serde_json::from_slice::<Value>(&details.bytes().await.unwrap()).unwrap()["error"],
        "request_not_observed"
    );
    assert_eq!(
        client
            .get(format!(
                "{base}/live/events?epoch={}&after=0",
                Uuid::new_v4()
            ))
            .header(header::COOKIE, cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let events = client
        .get(format!("{base}/live/events?epoch={epoch}&after=0"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    drop(events);
    for tail in [
        format!("{}/run", Uuid::new_v4()),
        format!("{}/run", epoch.simple()),
        "00000000-0000-0000-0000-000000000000/run".into(),
        format!("{epoch}/unknown"),
        format!("{epoch}/runs/{epoch}/run"),
        format!("{epoch}/application"),
        format!("{epoch}/assets/missing.js"),
    ] {
        assert_eq!(
            client
                .get(format!("{origin}/workbench/v1/runs/{tail}"))
                .header(header::COOKIE, cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
            "{tail}"
        );
    }
}
