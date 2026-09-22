use super::*;
use crate::workbench::decode::{Change, Decoded, TextKey};
use crate::workbench::live::LiveLimits;
use crate::workbench::redaction::RedactionPolicy;
use serde_json::{Value, json};
use std::time::Instant;

mod browser;
#[cfg(unix)]
mod concurrency_browser;
mod details;
#[cfg(unix)]
mod details_browser;
mod history;
mod history_browser;
#[cfg(unix)]
mod management;
#[cfg(unix)]
mod settings;
#[cfg(unix)]
mod settings_browser;
#[cfg(unix)]
mod terminal;
mod usage_browser;
#[cfg(unix)]
mod workspace;
#[cfg(unix)]
mod workspace_browser;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn cookie(client: &reqwest::Client, server: &ReadingServer) -> String {
    let response = client
        .post(format!("{}/workbench/v1/pair", server.state.origin))
        .header(header::ORIGIN, &server.state.origin)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"token":server.state.pairing}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    cookie.split(';').next().unwrap().to_owned()
}

fn publish(hub: &LiveHub, text: &str) {
    let request_id = Uuid::new_v4();
    hub.apply(Decoded {
        request_id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::TextDelta {
            key: TextKey {
                request_id,
                response_id: Some("resp_one".into()),
                wire_item_id: "msg_one".into(),
                content_index: 0,
            },
            text: RedactionPolicy::new(vec![]).unwrap().scrub(text),
        },
    });
}

#[tokio::test]
async fn independent_runs_do_not_overwrite_each_others_browser_pairing_cookie() {
    let first = ReadingServer::bind(LiveHub::new(LiveLimits::default()))
        .await
        .unwrap();
    let second = ReadingServer::bind(LiveHub::new(LiveLimits::default()))
        .await
        .unwrap();
    let client = client();
    let first_cookie = cookie(&client, &first).await;
    let second_cookie = cookie(&client, &second).await;
    assert!(
        first_cookie.split('=').next() != second_cookie.split('=').next(),
        "loopback cookies must be named per run because browser cookies are not scoped by port"
    );
    let both = format!("{first_cookie}; {second_cookie}");
    for server in [&first, &second] {
        let response = client
            .get(format!("{}/workbench/v1/run", server.state.origin))
            .header(header::COOKIE, &both)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn snapshot_work_is_bounded_until_the_response_body_is_consumed_or_dropped() {
    let server = ReadingServer::bind(LiveHub::new(LiveLimits::default()))
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, server.state.authority.parse().unwrap());
    headers.insert(
        header::COOKIE,
        cookie(&client(), &server).await.parse().unwrap(),
    );
    let first = snapshot(State(server.state.clone()), headers.clone()).await;
    let second = snapshot(State(server.state.clone()), headers.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        snapshot(State(server.state.clone()), headers.clone())
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(first);
    assert_eq!(
        snapshot(State(server.state.clone()), headers)
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn reading_routes_require_pairing_and_reject_cross_origin_and_host_spoofing() {
    let server = ReadingServer::bind(LiveHub::new(LiveLimits::default()))
        .await
        .unwrap();
    let client = client();
    let base = &server.state.origin;
    for route in [
        "run",
        "live/snapshot",
        &format!("live/events?epoch={}&after=0", server.state.hub.epoch()),
    ] {
        let response = client
            .get(format!("{base}/workbench/v1/{route}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let paired = cookie(&client, &server).await;
    for (name, value) in [
        (header::HOST.as_str(), "attacker.invalid"),
        (header::ORIGIN.as_str(), "https://attacker.invalid"),
        (header::ORIGIN.as_str(), "null"),
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-site", "same-site"),
    ] {
        let response = client
            .get(format!("{base}/workbench/v1/run"))
            .header(header::COOKIE, &paired)
            .header(name, value)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            !response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        );
    }
    for (origin, body, status) in [
        (
            None,
            json!({"token":server.state.pairing}).to_string(),
            StatusCode::FORBIDDEN,
        ),
        (
            Some(base),
            json!({"token":"incorrect-synthetic-secret"}).to_string(),
            StatusCode::FORBIDDEN,
        ),
        (
            Some(base),
            "invalid-json-private-fixture".into(),
            StatusCode::BAD_REQUEST,
        ),
        (Some(base), "x".repeat(1025), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let mut request = client.post(format!("{base}/workbench/v1/pair")).body(body);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), status);
        let error = response.text().await.unwrap();
        assert!(!error.contains("private-fixture") && !error.contains(&server.state.pairing));
    }
    let response = client
        .get(format!("{base}/workbench/v1/run"))
        .header(header::COOKIE, "workbench_session=incorrect")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = client
        .get(format!("{base}/workbench/v1/run"))
        .header(header::COOKIE, paired)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("typed-conversation-items")
    );
}

#[tokio::test]
async fn public_assets_have_strict_headers_and_no_embedded_pairing_or_session_secret() {
    assert!(
        matches!(
            WorkbenchAssets::get("workbench.html").unwrap().data,
            std::borrow::Cow::Borrowed(_)
        ),
        "debug binaries must keep their built frontend rather than read a later web/dist"
    );
    let server = ReadingServer::bind(LiveHub::new(LiveLimits::default()))
        .await
        .unwrap();
    let client = client();
    for route in ["/", "/workbench.js", "/workbench.css"] {
        let response = client
            .get(format!("{}{route}", server.state.origin))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_SECURITY_POLICY], CSP);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        let body = response.text().await.unwrap();
        assert!(!body.contains(&server.state.pairing) && !body.contains(&server.state.session));
    }
}

#[tokio::test]
async fn asset_paths_cannot_escape_the_built_asset_directory() {
    for path in [
        "../workbench.html",
        "../../src/workbench/reading/app.js",
        "..\\private.js",
        "./file.js",
        "nested//file.css",
    ] {
        assert_eq!(
            asset(Path(path.into())).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn authenticated_snapshot_and_content_sse_use_a_run_cursor_with_explicit_resnapshot() {
    let hub = LiveHub::new(LiveLimits::default());
    let server = ReadingServer::bind(hub.clone()).await.unwrap();
    let client = client();
    let cookie = cookie(&client, &server).await;
    let snapshot: Value = serde_json::from_str(
        &client
            .get(format!(
                "{}/workbench/v1/live/snapshot",
                server.state.origin
            ))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot["viewSeq"], 0);
    assert_eq!(snapshot["recorder"], "disabled");
    assert_eq!(snapshot["persistedThroughViewSeq"], 0);
    let mut stream = client
        .get(format!(
            "{}/workbench/v1/live/events?epoch={}&after=0",
            server.state.origin,
            hub.epoch()
        ))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(
        stream.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    publish(&hub, "网页中间正文");
    let bytes = stream.chunk().await.unwrap().unwrap();
    let event = std::str::from_utf8(&bytes).unwrap();
    assert!(
        event.contains("event: view")
            && event.contains("item.replace")
            && event.contains("网页中间正文")
    );
    for (epoch, after) in [(Uuid::new_v4(), 0), (hub.epoch(), 999)] {
        let response = client
            .get(format!(
                "{}/workbench/v1/live/events?epoch={epoch}&after={after}",
                server.state.origin
            ))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(response.text().await.unwrap().contains("snapshot_required"));
    }
}

#[tokio::test]
async fn overflowing_reader_is_closed_explicitly_and_does_not_block_new_snapshot() {
    let hub = LiveHub::new(LiveLimits {
        client_bytes: 64,
        ..LiveLimits::default()
    });
    let server = ReadingServer::bind(hub.clone()).await.unwrap();
    let client = client();
    let cookie = cookie(&client, &server).await;
    let mut stream = client
        .get(format!(
            "{}/workbench/v1/live/events?epoch={}&after=0",
            server.state.origin,
            hub.epoch()
        ))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    publish(&hub, "正文继续");
    let bytes = stream.chunk().await.unwrap().unwrap();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("snapshot_required")
    );
    assert!(stream.chunk().await.unwrap().is_none());
    let response = client
        .get(format!(
            "{}/workbench/v1/live/snapshot",
            server.state.origin
        ))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert!(response.text().await.unwrap().contains("正文继续"));
}
