use super::*;
use crate::workbench::config::{Config, ConfigService, Overrides, Prepared};

async fn setup() -> (tempfile::TempDir, ConfigService, ReadingServer) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&project).unwrap();
    let service = ConfigService::start(
        Prepared::load(
            &home,
            &project,
            &crate::workbench::paths::WorkbenchPaths::from_user_home(home.parent().unwrap())
                .unwrap(),
            None,
            Overrides::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let server = ReadingServer::bind_options(
        LiveHub::new(LiveLimits::default()),
        RuntimeOptions {
            settings: Some(service.handle()),
            ..RuntimeOptions::default()
        },
        None,
    )
    .await
    .unwrap();
    (dir, service, server)
}
#[tokio::test]
async fn settings_auth_origin_epoch_revision_and_json_boundaries_are_enforced() {
    let (_dir, _service, server) = setup().await;
    let client = client();
    let cookie = cookie(&client, &server).await;
    let url = format!("{}/workbench/v1/settings", server.state.origin);
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let response = client
        .get(&url)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let body =
        json!({"currentRunEpoch":server.state.hub.epoch(),"config":Config::default()}).to_string();
    let request = || {
        client
            .put(&url)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.clone())
    };
    assert_eq!(
        request()
            .header(header::IF_MATCH, &etag)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request()
            .header(header::ORIGIN, &server.state.origin)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        request()
            .header(header::ORIGIN, "https://elsewhere.invalid")
            .header(header::IF_MATCH, &etag)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request()
            .header(header::ORIGIN, &server.state.origin)
            .header(header::IF_MATCH, "*")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request()
            .header(header::ORIGIN, &server.state.origin)
            .header(header::IF_MATCH, &etag)
            .header(header::CONTENT_TYPE, "text/plain")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    let changed = json!({"currentRunEpoch":Uuid::new_v4(),"config":Config::default()}).to_string();
    assert_eq!(
        request()
            .header(header::ORIGIN, &server.state.origin)
            .header(header::IF_MATCH, &etag)
            .body(changed)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let invalid = format!(
        "{{\"currentRunEpoch\":\"{}\",\"config\":{{\"schemaVersion\":1,\"launch\":{{\"openBrowser\":\"SECRET\"}}}}}}",
        server.state.hub.epoch()
    );
    let response = request()
        .header(header::ORIGIN, &server.state.origin)
        .header(header::IF_MATCH, &etag)
        .body(invalid)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = response.text().await.unwrap();
    assert!(error.contains("launch.openBrowser"));
    assert!(!error.contains("SECRET"));
    assert_eq!(
        request()
            .header(header::ORIGIN, &server.state.origin)
            .header(header::IF_MATCH, &etag)
            .body(vec![b' '; 65 * 1024])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // Settings is only an API, never a separate frontend page.
    assert_eq!(
        client
            .get(format!("{}/settings", server.state.origin))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn settings_round_trip_and_external_conflicts_preserve_current_run() {
    let (dir, _service, server) = setup().await;
    let client = client();
    let cookie = cookie(&client, &server).await;
    let url = format!("{}/workbench/v1/settings", server.state.origin);
    let response = client
        .get(&url)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let mut config = Config::default();
    config.launch.open_browser = false;
    let request = |etag: &str, config: &Config| {
        client
            .put(&url)
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &server.state.origin)
            .header(header::IF_MATCH, etag)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"currentRunEpoch":server.state.hub.epoch(),"config":config}).to_string())
    };
    let response = request(&etag, &config).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let new_etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(etag, new_etag);
    let data: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(data["settings"]["saved"]["launch"]["openBrowser"], false);
    assert_eq!(data["settings"]["effective"]["launch"]["openBrowser"], true);
    assert_eq!(
        data["settings"]["restartRequired"],
        json!(["launch.openBrowser"])
    );
    assert_eq!(
        request(&etag, &Config::default())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PRECONDITION_FAILED
    );
    let path = dir.path().join(".codex-web/config/config.json");
    std::fs::write(&path, b"{").unwrap();
    let response = client
        .get(&url)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::ETAG).is_none());
    let data: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert!(data["settings"]["saved"].is_null());
    assert!(!data["settings"]["errors"].as_array().unwrap().is_empty());
    assert_eq!(
        data["settings"]["effective"]["history"]["cleanup"]["enabled"],
        false
    );
    assert_eq!(
        request(&new_etag, &Config::default())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(std::fs::read(path).unwrap(), b"{");
}
