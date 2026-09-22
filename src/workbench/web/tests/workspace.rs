use super::*;
use crate::workbench::workspace::Handle;

#[tokio::test]
async fn workspace_routes_pairing_path_method_and_query_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("safe 中文.txt"),
        "<script>synthetic literal</script>\n",
    )
    .unwrap();
    let server = ReadingServer::bind_options(
        LiveHub::new(LiveLimits::default()),
        RuntimeOptions {
            workspace: Some(Handle::start(dir.path()).unwrap()),
            ..RuntimeOptions::default()
        },
        None,
    )
    .await
    .unwrap();
    let client = client();
    let root = format!("{}/workbench/v1/workspace", server.state.origin);
    for route in [
        "files",
        "file?path=x",
        "search?q=hello",
        "git/status",
        "git/log",
        "git/diff?path=x&scope=working",
    ] {
        assert_eq!(
            client
                .get(format!("{root}/{route}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let cookie = cookie(&client, &server).await;
    let get = |route: &str| {
        client
            .get(format!("{root}/{route}"))
            .header(header::COOKIE, &cookie)
    };
    let files = get("files").send().await.unwrap().bytes().await.unwrap();
    let files: Value = serde_json::from_slice(&files).unwrap();
    assert_eq!(files["entries"][0]["name"], "safe 中文.txt");
    assert!(files["readAt"].is_string());
    assert_eq!(
        files["currentRunEpoch"],
        server.state.hub.epoch().to_string()
    );
    let file = get("file")
        .query(&[("path", "safe 中文.txt")])
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let file: Value = serde_json::from_slice(&file).unwrap();
    assert_eq!(file["text"], "<script>synthetic literal</script>\n");
    for path in [
        "../outside",
        "/etc/passwd",
        ".git/config",
        ".env",
        "a/../safe",
    ] {
        let response = get("file").query(&[("path", path)]).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response.text().await.unwrap();
        assert!(!body.contains(path));
        assert!(body.contains("workbench_workspace"));
    }
    for route in [
        "files?root=/",
        "search?q=x&executable=/bin/sh",
        "git/diff?path=safe&scope=all",
        "file?path=x&cursor=unused",
        "search?q=x&regex=invalid",
    ] {
        assert_eq!(
            get(route).send().await.unwrap().status(),
            StatusCode::BAD_REQUEST,
            "{route}"
        );
    }
    assert_eq!(
        get("files")
            .header(header::ORIGIN, "https://elsewhere.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get("files")
            .header(header::HOST, "wrong.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .put(format!("{root}/file"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &server.state.origin)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        get("git/status").send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let run = client
        .get(format!("{}/workbench/v1/run", server.state.origin))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let run: Value = serde_json::from_slice(&run).unwrap();
    assert_eq!(run["workspaceRoot"], dir.path().to_str().unwrap());
}
