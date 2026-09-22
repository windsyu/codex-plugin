use super::*;
use crate::workbench::{
    config::{ConfigService, Overrides, Prepared},
    recording::{Recorder, RecorderOptions},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_usage_preview_auth_scope_batch_and_stale_epoch_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let prepared = Prepared::load(
        &home,
        &cwd,
        &crate::workbench::paths::WorkbenchPaths::from_user_home(home.parent().unwrap()).unwrap(),
        None,
        Overrides::default(),
    )
    .unwrap();
    let root = prepared.data_dir.clone();
    let config = ConfigService::start(prepared).unwrap();
    let old = LiveHub::new(LiveLimits::default());
    drop(
        Recorder::start(
            &old,
            RecorderOptions::new(root.clone(), &cwd, "Synthetic old run".into()),
        )
        .unwrap(),
    );
    let hub = LiveHub::new(LiveLimits::default());
    let _recorder = Recorder::start(
        &hub,
        RecorderOptions::new(root, &cwd, "Synthetic current run".into()),
    )
    .unwrap();
    let server = ReadingServer::bind_options(
        hub.clone(),
        RuntimeOptions {
            settings: Some(config.handle()),
            ..RuntimeOptions::default()
        },
        None,
    )
    .await
    .unwrap();
    let client = client();
    let cookie = cookie(&client, &server).await;
    let origin = &server.state.origin;
    let usage = format!("{origin}/workbench/v1/history/usage");
    assert_eq!(
        client.get(&usage).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let value = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = client
                .get(&usage)
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                let v = json_body(response).await;
                if v["result"]["state"] != "scanning" {
                    break v;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(value["currentRunEpoch"], hub.epoch().to_string());
    assert!(value["result"]["runCount"].as_u64().unwrap() >= 1);
    let route = format!("{origin}/workbench/v1/history/cleanup/preview");
    let body =
        json!({"currentRunEpoch":hub.epoch(),"runEpochs":[old.epoch(),hub.epoch()]}).to_string();
    let request = || {
        client
            .post(&route)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
    };
    assert_eq!(
        request().body(body.clone()).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let wrong = json!({"currentRunEpoch":Uuid::new_v4(),"runEpochs":[old.epoch()]}).to_string();
    assert_eq!(
        request()
            .header(header::ORIGIN, origin)
            .body(wrong)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let too_many=json!({"currentRunEpoch":hub.epoch(),"runEpochs":(0..101).map(|_|Uuid::new_v4()).collect::<Vec<_>>()}).to_string();
    assert_eq!(
        request()
            .header(header::ORIGIN, origin)
            .body(too_many)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let response = request()
        .header(header::ORIGIN, origin)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let id = json_body(response).await["result"]["previewId"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("{origin}/workbench/v1/history/cleanup/previews/{id}");
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let v = json_body(
                client
                    .get(&path)
                    .header(header::COOKIE, &cookie)
                    .send()
                    .await
                    .unwrap(),
            )
            .await;
            if v["result"]["status"] == "ready" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(result["result"]["items"][0]["eligible"], true);
    assert_eq!(result["result"]["items"][1]["eligible"], false);
    assert_eq!(result["result"]["executable"], false);
    assert_eq!(
        client
            .get(format!(
                "{origin}/workbench/v1/history/cleanup/previews/{}",
                Uuid::new_v4()
            ))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(
        home.parent()
            .unwrap()
            .join(".codex-web/history/runs")
            .join(old.epoch().to_string())
            .exists()
    );
    let operation = Uuid::new_v4();
    let make_job = |preview: &Value, revision: &str| {
        json!({"currentRunEpoch":hub.epoch(),"previewId":preview["result"]["previewId"],"configRevision":revision,"operationId":operation}).to_string()
    };
    let route_jobs = format!("{origin}/workbench/v1/history/cleanup/jobs");
    let before = config.handle().read().await.unwrap();
    let body = make_job(&result, before.revision.as_deref().unwrap());
    assert_eq!(
        client
            .post(&route_jobs)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, origin)
            .body(body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut value = before.saved.unwrap();
    value.history.cleanup.enabled = true;
    config
        .handle()
        .save(before.revision.unwrap(), value)
        .await
        .unwrap();
    let preview = json_body(
        request()
            .header(header::ORIGIN, origin)
            .body(json!({"currentRunEpoch":hub.epoch(),"runEpochs":[old.epoch()]}).to_string())
            .send()
            .await
            .unwrap(),
    )
    .await;
    let id = preview["result"]["previewId"].as_str().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let v = json_body(
                client
                    .get(format!(
                        "{origin}/workbench/v1/history/cleanup/previews/{id}"
                    ))
                    .header(header::COOKIE, &cookie)
                    .send()
                    .await
                    .unwrap(),
            )
            .await;
            if v["result"]["status"] == "ready" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let body = make_job(
        &result,
        result["result"]["configRevision"].as_str().unwrap(),
    );
    for _ in 0..2 {
        assert_eq!(
            client
                .post(&route_jobs)
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, origin)
                .body(body.clone())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let v = json_body(
                client
                    .get(format!("{route_jobs}/{operation}"))
                    .header(header::COOKIE, &cookie)
                    .send()
                    .await
                    .unwrap(),
            )
            .await;
            if v["result"]["status"] == "complete" {
                assert_eq!(v["result"]["items"][0]["state"], "deleted");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client
            .get(format!(
                "{origin}/workbench/v1/history/{}/status",
                old.epoch()
            ))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
}

async fn json_body(response: reqwest::Response) -> Value {
    serde_json::from_str(&response.text().await.unwrap()).unwrap()
}
