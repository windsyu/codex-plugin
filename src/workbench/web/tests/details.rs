use super::*;
use crate::workbench::decode::{DiagnosticCode, ResponseStatus, details};

fn context(hub: &LiveHub, id: Uuid, body: Value, index: Option<u64>) {
    hub.apply(Decoded {
        request_id: id,
        capture_seq: 3,
        received_at: Instant::now(),
        change: Change::Document {
            document: details::request(
                &body,
                index,
                &RedactionPolicy::new(vec!["fixture-sensitive-value".into()]).unwrap(),
            ),
        },
    });
}

#[tokio::test]
async fn details_route_requires_pairing_origin_and_epoch_and_validates_query_without_echoing() {
    let hub = LiveHub::new(LiveLimits::default());
    let server = ReadingServer::bind(hub.clone()).await.unwrap();
    let client = client();
    let id = Uuid::new_v4();
    context(
        &hub,
        id,
        json!({"instructions":"fixture-sensitive-value","input":[{"type":"input_image","image_url":"PRIVATE_MEDIA"}]}),
        None,
    );
    let base = format!("{}/workbench/v1/requests/{id}", server.state.origin);
    let url = format!("{base}?epoch={}", hub.epoch());
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let paired = cookie(&client, &server).await;
    for (key, value) in [
        (header::HOST.as_str(), "attacker.invalid"),
        (header::ORIGIN.as_str(), "https://attacker.invalid"),
        ("sec-fetch-site", "cross-site"),
    ] {
        assert_eq!(
            client
                .get(&url)
                .header(header::COOKIE, &paired)
                .header(key, value)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    for (query, expected) in [
        (String::new(), StatusCode::BAD_REQUEST),
        ("epoch=PRIVATE_MALFORMED".into(), StatusCode::BAD_REQUEST),
        (format!("epoch={}", Uuid::new_v4()), StatusCode::CONFLICT),
        (
            format!("epoch={}&extra=PRIVATE_EXTRA", hub.epoch()),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("epoch={}&epoch={}", hub.epoch(), hub.epoch()),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("epoch={}&cursor={}", hub.epoch(), "x".repeat(161)),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = client
            .get(format!("{base}?{query}"))
            .header(header::COOKIE, &paired)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert!(!response.text().await.unwrap().contains("PRIVATE_"));
    }
    let response = client
        .get(&url)
        .header(header::COOKIE, &paired)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let text = response.text().await.unwrap();
    assert!(text.contains("[已脱敏]"));
    assert!(!text.contains("fixture-sensitive-value") && !text.contains("PRIVATE_MEDIA"));
    let page: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(page["runEpoch"], hub.epoch().to_string());
    assert_eq!(page["requestId"], id.to_string());
    assert_eq!(page["omitted"], true);
}

#[tokio::test]
async fn details_pages_have_scoped_cursors_conflicts_and_explicit_missing_states() {
    let hub = LiveHub::new(LiveLimits::default());
    let server = ReadingServer::bind(hub.clone()).await.unwrap();
    let client = client();
    let paired = cookie(&client, &server).await;
    let id = Uuid::new_v4();
    let url = |id| {
        format!(
            "{}/workbench/v1/requests/{id}?epoch={}",
            server.state.origin,
            hub.epoch()
        )
    };
    let get = |url: String| client.get(url).header(header::COOKIE, &paired).send();
    assert_eq!(get(url(id)).await.unwrap().status(), StatusCode::NOT_FOUND);
    hub.apply(Decoded {
        request_id: id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::Response {
            response_id: Some("response".into()),
            status: ResponseStatus::Receiving,
            model: None,
            usage: None,
        },
    });
    let page: Value =
        serde_json::from_str(&get(url(id)).await.unwrap().text().await.unwrap()).unwrap();
    assert_eq!(page["availability"], "pending");
    hub.apply(Decoded {
        request_id: id,
        capture_seq: 2,
        received_at: Instant::now(),
        change: Change::Diagnostic {
            code: DiagnosticCode::ObservationGap,
        },
    });
    let page: Value =
        serde_json::from_str(&get(url(id)).await.unwrap().text().await.unwrap()).unwrap();
    assert_eq!(page["availability"], "unavailable");
    context(
        &hub,
        id,
        json!({"input":(0..40).map(|n|json!({"content":format!("{n}")})).collect::<Vec<_>>()}),
        None,
    );
    let page: Value =
        serde_json::from_str(&get(url(id)).await.unwrap().text().await.unwrap()).unwrap();
    assert_eq!(page["availability"], "captured");
    assert_eq!(page["entries"].as_array().unwrap().len(), 16);
    assert_eq!(page["requestCaptured"], true);
    assert_eq!(page["responseCaptured"], false);
    let cursor = page["nextCursor"].as_str().unwrap();
    let second = get(format!("{}&cursor={cursor}", url(id))).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second: Value = serde_json::from_str(&second.text().await.unwrap()).unwrap();
    assert_eq!(second["entries"][0]["position"], 15);
    let other = Uuid::new_v4();
    context(&hub, other, json!({"input":[]}), None);
    assert_eq!(
        get(format!("{}&cursor={cursor}", url(other)))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    context(&hub, id, json!({"input":["conflicting request"]}), None);
    assert_eq!(
        get(format!("{}&cursor={cursor}", url(id)))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let page: Value =
        serde_json::from_str(&get(url(id)).await.unwrap().text().await.unwrap()).unwrap();
    assert_eq!(page["conflict"], true);
    for _ in 0..65 {
        context(&hub, Uuid::new_v4(), json!({"input":[]}), None);
    }
    assert_eq!(get(url(id)).await.unwrap().status(), StatusCode::GONE);
}

#[tokio::test]
async fn details_responses_share_bounded_reading_slots_until_body_drop_and_pages_are_bounded() {
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    context(
        &hub,
        id,
        json!({"input":vec![json!({"content":"\\\"\n中".repeat(10000)});100]}),
        None,
    );
    let server = ReadingServer::bind(hub.clone()).await.unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, server.state.authority.parse().unwrap());
    headers.insert(
        header::COOKIE,
        cookie(&client(), &server).await.parse().unwrap(),
    );
    let request = || {
        details_api::read(
            State(server.state.clone()),
            headers.clone(),
            Path(id.to_string()),
            Query::try_from_uri(&format!("/?epoch={}", hub.epoch()).parse().unwrap()),
        )
    };
    let first = request().await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = snapshot(State(server.state.clone()), headers.clone()).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(request().await.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(first.into_body(), 128 * 1024)
        .await
        .unwrap();
    assert!(bytes.len() <= 128 * 1024);
    let page: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(page["truncated"], true);
    assert!(page["nextCursor"].is_string());
    let third = request().await;
    assert_eq!(third.status(), StatusCode::OK);
    drop(third);
    assert_eq!(request().await.status(), StatusCode::OK);
}
