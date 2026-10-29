use super::*;
use crate::workbench::proxy::WorkspaceGuard;
use axum::{Json, Router, routing::get};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "installed official CLI; isolated homes and synthetic loopback accounts/check only"]
async fn installed_cli_workspace_route_reader_and_auth_generation_fence() {
    let binary = std::env::var("WORKBENCH_TEST_CODEX").expect("set installed CLI path");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    let user = temp.path().join("user");
    let cwd = temp.path().join("project");
    for directory in [&home, &user, &cwd] {
        std::fs::create_dir(directory).unwrap();
    }
    let generation = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(AtomicUsize::new(0));
    let current = generation.clone();
    let requests = seen.clone();
    let app=Router::new().route("/api/codex/accounts/check",get(move || {
        let current=current.clone();let requests=requests.clone();async move {
            requests.fetch_add(1,Ordering::SeqCst);
            Json(json!({"accounts":[{"id":"synthetic-account","workspace_backend_origin": if current.load(Ordering::SeqCst)==0 {"https://first.example"} else {"https://second.example"},"account_routing_override":"us"}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    std::fs::write(
        home.join("config.toml"),
        format!("chatgpt_base_url='http://{address}'\ncli_auth_credentials_store='file'\n"),
    )
    .unwrap();
    let claims = json!({"email":"synthetic@example.invalid","https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account","chatgpt_plan_type":"pro"}});
    let token = format!(
        "{}.{}.signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    let write_auth = |access: &str| {
        std::fs::write(home.join("auth.json"),serde_json::to_vec(&json!({"auth_mode":"chatgpt","tokens":{"id_token":token,"access_token":access,"refresh_token":"synthetic-refresh","account_id":"synthetic-account"},"last_refresh":chrono::Utc::now().to_rfc3339()})).unwrap()).unwrap();
    };
    write_auth("synthetic-initial");
    let quote = |path: &str| format!("'{}'", path.replace('\'', "'\\''"));
    let wrapper = temp.path().join("isolated-cli");
    std::fs::write(&wrapper,format!("#!/bin/sh\nexport HOME={} USERPROFILE={}\nexport HTTP_PROXY=http://127.0.0.1:1 HTTPS_PROXY=http://127.0.0.1:1 NO_PROXY=127.0.0.1\nunset OPENAI_API_KEY CODEX_API_KEY CODEX_ACCESS_TOKEN\nexec {} \"$@\"\n",quote(user.to_str().unwrap()),quote(user.to_str().unwrap()),quote(&binary))).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let route = read(&wrapper, &home, &cwd, None).await.unwrap();
    assert_eq!(route.url, "https://first.example/backend-api/codex");
    let guard =
        crate::workbench::launch::route_guard::Guard::new(&wrapper, &home, &cwd, None, &route);
    let initial = guard.check().await.unwrap();
    assert!(initial.unchanged());
    let count = seen.load(Ordering::SeqCst);
    guard.check().await.unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        count,
        "unchanged credentials must not trigger network discovery"
    );
    write_auth("synthetic-ordinary-refresh");
    assert!(!initial.unchanged());
    let refreshed = guard.check().await.unwrap();
    assert!(refreshed.unchanged());
    assert!(seen.load(Ordering::SeqCst) > count);
    generation.store(1, Ordering::SeqCst);
    write_auth("synthetic-same-account-new-route");
    assert!(
        guard.check().await.is_err(),
        "same-account new-origin credentials must not reach the old route"
    );
    server.abort();
}

#[tokio::test]
async fn helper_route_is_rejected_when_source_changes_between_metadata_reads() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("config.toml"), "model_provider='gateway'\n").unwrap();
    let wrapper = temp.path().join("synthetic-cli");
    std::fs::write(&wrapper,r#"#!/bin/sh
export HOME="$CODEX_HOME" USERPROFILE="$CODEX_HOME"
while IFS= read -r line; do
case "$line" in
 *'"id":1,'*) printf '%s\n' '{"id":1,"result":{}}' ;;
 *'"id":2,'*) printf '%s\n' '{"id":2,"result":{"config":{"model_provider":"gateway","model_providers":{"gateway":{"base_url":"https://old.example/v1"}}},"layers":[]}}' ;;
 *'"id":3,'*) printf '%s\n' '{"id":3,"result":{"requirements":null}}' ;;
 *'"id":4,'*) printf '%s\n' "model_provider='new-provider'" > "$CODEX_HOME/config.toml"; printf '%s\n' '{"id":4,"result":{"account":null}}' ;;
 *'"id":5,'*) printf '%s\n' '{"id":5,"result":{"config":{"model_provider":"gateway","model_providers":{"gateway":{"base_url":"https://old.example/v1"}}},"layers":[]}}' ;;
esac
done
"#).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(read(&wrapper, &home, temp.path(), None).await.is_err());
}
