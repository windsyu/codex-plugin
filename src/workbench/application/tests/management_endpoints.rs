use super::*;
use crate::workbench::application::management::{self, Action};

fn token(app: &Application) -> String {
    reqwest::Url::parse(&app.entry.url)
        .unwrap()
        .fragment()
        .unwrap()
        .strip_prefix("pair=")
        .unwrap()
        .to_owned()
}

fn post(app: &Application, path: &str, value: Value) -> reqwest::RequestBuilder {
    client()
        .post(format!("{}{path}", app.entry.address))
        .header("Origin", &app.entry.address)
        .header("Content-Type", "application/json")
        .body(value.to_string())
}

#[tokio::test]
async fn lifecycle_requires_private_capability_origin_and_current_instance() {
    let fixture = Fixture::new();
    let app = fixture.app(Overrides::default());
    let cookie = pair(&app).await;
    let path = "/workbench/v1/application/control";
    let valid = json!({"instanceId":app.instance_id,"token":token(&app),"action":"status"});
    let mut wrong = valid.clone();
    wrong["token"] = "wrong".into();
    assert_eq!(
        post(&app, path, wrong)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut stale = valid.clone();
    stale["instanceId"] = Uuid::new_v4().to_string().into();
    assert_eq!(post(&app, path, stale).send().await.unwrap().status(), 409);
    assert_eq!(
        post(&app, path, valid.clone())
            .header("Origin", "https://evil.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client()
            .post(format!("{}{path}", app.entry.address))
            .header("Content-Type", "application/json")
            .body(valid.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        post(&app, path, valid)
            .header("Content-Type", "text/plain")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(!app.shutdown_requested());
    app.set_service_label("synthetic-service".into());
    let status = management::control(&app.entry, Action::Status)
        .await
        .unwrap();
    assert_eq!(status.instance_id, app.instance_id);
    assert_eq!(status.active_runs, 0);
    assert_eq!(status.service_label.as_deref(), Some("synthetic-service"));
    assert!(status.registration_required());
    let text = serde_json::to_string(&status).unwrap();
    assert!(!text.contains(&token(&app)));
    assert!(!text.contains("bindingCode"));
}

#[tokio::test]
async fn private_shutdown_closes_admission_and_application_drop_reaps_all_projects() {
    let fixture = Fixture::new();
    let app = fixture.app(Overrides {
        codex_bin: Some(fixture.cli()),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let mut children = Vec::new();
    for project in [fixture.cwd.clone(), fixture.dir.path().to_path_buf()] {
        children.push(
            connect(
                &app.entry,
                Connect {
                    project: Some(project),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap()
            .cli_pid,
        );
    }
    assert_eq!(
        management::control(&app.entry, Action::Status)
            .await
            .unwrap()
            .active_runs,
        2
    );
    let status = management::control(&app.entry, Action::Shutdown)
        .await
        .unwrap();
    assert!(status.stopping);
    assert!(app.shutdown_requested());
    let info = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
    assert_eq!(info["launchAvailable"], false);
    assert_eq!(
        post(
            &app,
            "/workbench/v1/application/connect",
            json!({"instanceId":app.instance_id,"token":token(&app)})
        )
        .send()
        .await
        .unwrap()
        .status(),
        503
    );
    for path in [
        "/workbench/v1/runs",
        "/workbench/v1/launch-targets",
        "/workbench/v1/application/directories",
        "/workbench/v1/application/pick-directory",
    ] {
        assert_eq!(
            post(&app, path, json!({}))
                .header("Cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            503,
            "{path}"
        );
    }
    drop(app);
    for pid in children {
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            -1,
            "owned CLI must exit"
        );
    }
}

#[tokio::test]
async fn directory_api_uses_injected_home_owner_and_instance_boundaries() {
    let fixture = Fixture::new();
    let app = fixture.app(Overrides::default());
    let cookie = pair(&app).await;
    let path = "/workbench/v1/application/directories";
    let valid = json!({"instanceId":app.instance_id});
    assert_eq!(
        post(&app, path, valid.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        post(&app, path, valid.clone())
            .header("Cookie", &cookie)
            .header("Origin", "https://evil.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        post(&app, path, json!({"instanceId":Uuid::new_v4()}))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let response = post(&app, path, valid)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(
        body["homePath"],
        fixture.dir.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(body["currentPath"], body["homePath"]);
    assert!(
        body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "项目 with spaces")
    );
    assert!(
        !fixture.home.exists(),
        "browsing cannot initialize native CLI data"
    );
    let protected = fixture.dir.path().join("protected");
    std::fs::create_dir(&protected).unwrap();
    std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o000)).unwrap();
    let response = post(
        &app,
        path,
        json!({"instanceId":app.instance_id,"path":protected}),
    )
    .header("Cookie", &cookie)
    .send()
    .await
    .unwrap();
    std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        response.status(),
        422,
        "OS access failure must not revoke remote login"
    );
    assert_eq!(
        json_body(response).await["error"]["code"],
        "directory_forbidden"
    );
}
