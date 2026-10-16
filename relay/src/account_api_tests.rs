use super::*;
use crate::{
    Config, Store,
    store::{digest, secret},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
    response::Response,
};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    router: Router,
    admin: Uuid,
    user: Uuid,
    token: String,
    user_token: String,
    credential: String,
    gateway: Uuid,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let admin = Uuid::new_v4();
        let user = Uuid::new_v4();
        let token = secret();
        let user_token = secret();
        let credential = secret();
        let gateway = Uuid::new_v4();
        {
            let c = store.0.lock().unwrap();
            // The role, not the reserved username, grants administrator privileges.
            c.execute(
                "INSERT INTO accounts(id,username,role) VALUES(?1,'operator','admin')",
                [admin.to_string()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO accounts(id,username,role) VALUES(?1,'admin','user')",
                [user.to_string()],
            )
            .unwrap();
            for (account, token) in [(admin, &token), (user, &user_token)] {
                c.execute(
                    "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
                    params![
                        Uuid::new_v4().to_string(),
                        digest(token),
                        account.to_string(),
                        now() + 3600
                    ],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO gateways(id,account,name,credential) VALUES(?1,?2,'fixture',?3)",
                params![gateway.to_string(), user.to_string(), digest(&credential)],
            )
            .unwrap();
        }
        let router = crate::app(Config {
            data_dir: dir.path().to_owned(),
            portal_origin: "http://localhost:8123".into(),
            workbench_domain: String::new(),
            workbench_origin: None,
            fixed_gateway_id: None,
            single_origin: true,
            password_workers: 4,
            allow_insecure_loopback: true,
            trusted_client_ip_header: None,
        })
        .unwrap();
        Self {
            _dir: dir,
            store,
            router,
            admin,
            user,
            token,
            user_token,
            credential,
            gateway,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: serde_json::Value,
    ) -> Response {
        self.router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "localhost:8123")
                    .header("origin", "http://localhost:8123")
                    .header("content-type", "application/json")
                    .header("cookie", format!("__Host-relay_session={token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}
async fn json(response: Response) -> serde_json::Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap()
}

#[tokio::test]
async fn delete_user_removes_credentials_and_registration_but_keeps_audit() {
    let f = Fixture::new();
    let parent = f.store.session(&f.user_token, None).unwrap();
    let child = Uuid::new_v4();
    {
        let c = f.store.0.lock().unwrap();
        c.execute("INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,'child-hash',?2,?3,?4,?5)",params![child.to_string(),f.user.to_string(),f.gateway.to_string(),parent.id.to_string(),now()+3600]).unwrap();
        c.execute(
            "INSERT INTO tokens VALUES('legacy-token',?1,'reset',?2)",
            params![f.user.to_string(), now() + 3600],
        )
        .unwrap();
        c.execute(
            "INSERT INTO open_codes VALUES('open-code',?1,?2,?3)",
            params![parent.id.to_string(), f.gateway.to_string(), now() + 3600],
        )
        .unwrap();
        audit(
            &c,
            "gateway_bound",
            Some(&f.user.to_string()),
            Some(&f.gateway.to_string()),
        )
        .unwrap();
    }
    let path = format!("/relay/v1/admin/users/{}", f.user);
    assert_eq!(
        f.request("DELETE", &path, &f.token, serde_json::json!({}))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert!(f.store.session(&f.user_token, None).is_none());
    assert!(!f.store.active(child));
    assert!(f.store.gateway_auth(&f.credential).is_none());
    {
        let c = f.store.0.lock().unwrap();
        for table in ["gateways", "sessions", "tokens", "open_codes"] {
            let count: i64 = c
                .query_row(
                    &format!(
                        "SELECT count(*) FROM {table}{}",
                        if table == "sessions" {
                            " WHERE account != (SELECT id FROM accounts WHERE role='admin')"
                        } else {
                            ""
                        }
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM audit WHERE account=?1",
                [f.user.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM accounts WHERE id=?1",
                [f.user.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    assert_eq!(
        f.request("DELETE", &path, &f.token, serde_json::json!({}))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let created = f
        .request(
            "POST",
            "/relay/v1/admin/users",
            &f.token,
            serde_json::json!({"username":"admin","temporaryPassword":"summer maple ninety 543"}),
        )
        .await;
    assert_eq!(created.status(), StatusCode::OK);
    assert_ne!(json(created).await["accountId"], f.user.to_string());
    assert!(f.store.gateway_auth(&f.credential).is_none());
    assert!(f.store.session(&f.user_token, None).is_none());
}

#[tokio::test]
async fn delete_user_requires_origin_and_rejects_all_administrators() {
    let f = Fixture::new();
    let other_admin = Uuid::new_v4();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO accounts(id,username,role) VALUES(?1,'other_operator','admin')",
            [other_admin.to_string()],
        )
        .unwrap();
    for id in [f.admin, other_admin] {
        assert_eq!(
            f.request(
                "DELETE",
                &format!("/relay/v1/admin/users/{id}"),
                &f.token,
                serde_json::json!({})
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    for origin in [None, Some("https://attacker.invalid")] {
        let mut request = Request::builder()
            .method("DELETE")
            .uri(format!("/relay/v1/admin/users/{}", f.user))
            .header("host", "localhost:8123")
            .header("cookie", format!("__Host-relay_session={}", f.token));
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        assert_eq!(
            f.router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert!(f.store.session(&f.user_token, None).is_some());
    assert_eq!(
        f.request(
            "POST",
            "/relay/v1/admin/users",
            &f.token,
            serde_json::json!({"username":"windsyu","temporaryPassword":"summer maple ninety 543"})
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn failed_delete_audit_rolls_back_every_removed_record() {
    let f = Fixture::new();
    f.store.0.lock().unwrap().execute_batch("CREATE TRIGGER reject_delete_audit BEFORE INSERT ON audit WHEN NEW.event='admin_user_deleted' BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END").unwrap();
    assert_eq!(
        f.request(
            "DELETE",
            &format!("/relay/v1/admin/users/{}", f.user),
            &f.token,
            serde_json::json!({})
        )
        .await
        .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(f.store.session(&f.user_token, None).is_some());
    assert!(f.store.gateway_auth(&f.credential).is_some());
}

#[tokio::test]
async fn delete_rechecks_administrator_after_concurrent_revocation() {
    let f = Fixture::new();
    let gate = std::sync::Arc::new(auth::LoginGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    DELETE_GATE.lock().unwrap().insert(f.user, gate.clone());
    let router = f.router.clone();
    let path = format!("/relay/v1/admin/users/{}", f.user);
    let token = f.token.clone();
    let delete = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(path)
                    .header("host", "localhost:8123")
                    .header("origin", "http://localhost:8123")
                    .header("cookie", format!("__Host-relay_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    {
        let mut c = f.store.0.lock().unwrap();
        let tx = c.transaction().unwrap();
        revoke_account(&tx, &f.admin.to_string()).unwrap();
        tx.commit().unwrap();
    }
    gate.release.notify_one();
    assert_eq!(delete.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    DELETE_GATE.lock().unwrap().remove(&f.user);
    assert!(f.store.session(&f.user_token, None).is_some());
    assert!(f.store.gateway_auth(&f.credential).is_some());
}

#[tokio::test]
async fn approval_captured_before_delete_cannot_create_a_gateway() {
    let f = Fixture::new();
    let pending = json(
        f.request(
            "POST",
            "/relay/v1/bindings",
            "",
            serde_json::json!({"name":"synthetic new machine"}),
        )
        .await,
    )
    .await;
    let gate = std::sync::Arc::new(auth::LoginGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    crate::portal::APPROVAL_GATE
        .lock()
        .unwrap()
        .insert(f.user, gate.clone());
    let router = f.router.clone();
    let token = f.user_token.clone();
    let approve = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/relay/v1/bindings/approve")
                    .header("host", "localhost:8123")
                    .header("origin", "http://localhost:8123")
                    .header("content-type", "application/json")
                    .header("cookie", format!("__Host-relay_session={token}"))
                    .body(Body::from(
                        serde_json::json!({"userCode":pending["userCode"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        f.request(
            "DELETE",
            &format!("/relay/v1/admin/users/{}", f.user),
            &f.token,
            serde_json::json!({})
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    gate.release.notify_one();
    assert_eq!(approve.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    crate::portal::APPROVAL_GATE.lock().unwrap().remove(&f.user);
    assert_eq!(
        f.store
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM gateways", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn login_verified_before_deletion_cannot_authorize_recreated_username() {
    let f = Fixture::new();
    let username = format!("race_delete_{}", Uuid::new_v4().simple());
    let password = "synthetic deleted login phrase 2873";
    let encoded = auth::hash_password_sync(password).unwrap();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET username=?1,password=?2 WHERE id=?3",
            params![username, encoded, f.user.to_string()],
        )
        .unwrap();
    let gate = std::sync::Arc::new(auth::LoginGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    auth::LOGIN_GATE
        .lock()
        .unwrap()
        .insert(username.clone(), gate.clone());
    let router = f.router.clone();
    let login_username = username.clone();
    let login = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/relay/v1/login")
                    .header("host", "localhost:8123")
                    .header("origin", "http://localhost:8123")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username":login_username,"password":password})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), gate.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        f.request(
            "DELETE",
            &format!("/relay/v1/admin/users/{}", f.user),
            &f.token,
            serde_json::json!({})
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let new_id = Uuid::new_v4();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO accounts(id,username,password) VALUES(?1,?2,?3)",
            params![new_id.to_string(), username, encoded],
        )
        .unwrap();
    gate.release.notify_one();
    let response = login.await.unwrap();
    auth::LOGIN_GATE.lock().unwrap().remove(&username);
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(
        f.store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sessions WHERE account=?1",
                [new_id.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn gateway_waiting_for_first_handshake_is_not_reported_as_incompatible() {
    let f = Fixture::new();
    let list = json(
        f.request(
            "GET",
            "/relay/v1/gateways",
            &f.user_token,
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    assert_eq!(list["gateways"][0]["status"], "awaitingConnection");
    assert_eq!(list["gateways"][0]["online"], false);
    assert_eq!(list["gateways"][0]["versionIncompatible"], false);
    for (api, expected) in [
        (99, "versionIncompatible"),
        (1, "versionIncompatible"),
        (codex_relay_protocol::WORKBENCH_API_VERSION, "offline"),
    ] {
        f.store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE gateways SET workbench_api_version=?1 WHERE id=?2",
                params![api, f.gateway.to_string()],
            )
            .unwrap();
        let list = json(
            f.request(
                "GET",
                "/relay/v1/gateways",
                &f.user_token,
                serde_json::json!({}),
            )
            .await,
        )
        .await;
        assert_eq!(list["gateways"][0]["status"], expected);
    }
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE gateways SET auth_generation=0,workbench_api_version=0 WHERE id=?1",
            [f.gateway.to_string()],
        )
        .unwrap();
    let list = json(
        f.request(
            "GET",
            "/relay/v1/gateways",
            &f.user_token,
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    assert_eq!(list["gateways"][0]["status"], "reauthenticationRequired");
}

#[tokio::test]
async fn portal_documents_and_central_deep_links_are_exact_and_owned() {
    let f = Fixture::new();
    for path in [
        "/",
        "/login",
        "/computers",
        "/authorize",
        "/account/security",
        "/admin/users",
    ] {
        let response = f.request("GET", path, "", serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("script-src 'self'")
        );
    }
    let css = f
        .request("GET", "/relay.css", "", serde_json::json!({}))
        .await;
    assert_eq!(css.status(), StatusCode::OK);
    assert!(
        css.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );
    let run = Uuid::new_v4();
    let pages = codex_relay_protocol::APPLICATION_PAGES
        .iter()
        .map(|p| p.to_string())
        .chain(
            codex_relay_protocol::RUN_PAGES
                .iter()
                .map(|p| format!("/runs/{run}/{p}")),
        );
    for page in pages {
        let path = format!("/g/{}{page}", f.gateway);
        let response = f
            .request("GET", &path, &f.user_token, serde_json::json!({}))
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        assert_eq!(
            f.request("GET", &path, &f.token, serde_json::json!({}))
                .await
                .status(),
            StatusCode::NOT_FOUND,
            "admin must not enter another user's computer: {path}"
        );
        assert_eq!(
            f.request("GET", &path, "", serde_json::json!({}))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for page in [
        "/relay".to_string(),
        "/unknown".to_string(),
        format!("/runs/{run}/unknown"),
        format!("/runs/{run}/chat/extra"),
        format!("/runs/{}/chat", Uuid::nil()),
        format!("/runs/{}/chat", run.simple()),
    ] {
        let path = format!("/g/{}{page}", f.gateway);
        let response = f
            .request("GET", &path, &f.user_token, serde_json::json!({}))
            .await;
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
            ),
            "{path}: {}",
            response.status()
        );
    }
}

#[tokio::test]
async fn admin_role_and_forced_change_are_enforced_for_every_management_route() {
    let f = Fixture::new();
    let cases = vec![
        (
            "GET",
            "/relay/v1/admin/users".to_string(),
            serde_json::json!({}),
        ),
        (
            "POST",
            "/relay/v1/admin/users".to_string(),
            serde_json::json!({"username":"bob","temporaryPassword":"summer maple ninety 543"}),
        ),
        (
            "POST",
            format!("/relay/v1/admin/users/{}/password", f.user),
            serde_json::json!({"temporaryPassword":"summer maple ninety 543"}),
        ),
        (
            "POST",
            format!("/relay/v1/admin/users/{}/disable", f.user),
            serde_json::json!({}),
        ),
        (
            "POST",
            format!("/relay/v1/admin/users/{}/enable", f.user),
            serde_json::json!({}),
        ),
        (
            "DELETE",
            format!("/relay/v1/admin/users/{}", f.user),
            serde_json::json!({}),
        ),
    ];
    for (method, path, body) in &cases {
        assert_eq!(
            f.request(method, path, &f.user_token, body.clone())
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        f.request(
            "GET",
            "/relay/v1/admin/users",
            &f.token,
            serde_json::json!({})
        )
        .await
        .status(),
        StatusCode::OK
    );
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET must_change_password=1 WHERE id=?1",
            [f.admin.to_string()],
        )
        .unwrap();
    for (method, path, body) in &cases {
        assert_eq!(
            f.request(method, path, &f.token, body.clone())
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        f.request("GET", "/relay/v1/account", &f.token, serde_json::json!({}))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        f.request("GET", "/relay/v1/gateways", &f.token, serde_json::json!({}))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn admin_reset_disable_enable_preserve_ownership_and_revoke_credentials() {
    let f = Fixture::new();
    for action in ["password", "disable", "enable"] {
        let response = f
            .request(
                "POST",
                &format!("/relay/v1/admin/users/{}/{action}", f.admin),
                &f.token,
                serde_json::json!({"temporaryPassword":"summer maple ninety 543"}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(f.store.gateway_auth(&f.credential).is_some());
    let response = f
        .request(
            "POST",
            &format!("/relay/v1/admin/users/{}/password", f.user),
            &f.token,
            serde_json::json!({"temporaryPassword":"summer maple ninety 543"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(f.store.session(&f.user_token, None).is_none());
    assert!(f.store.gateway_auth(&f.credential).is_none());
    for action in ["disable", "enable"] {
        assert_eq!(
            f.request(
                "POST",
                &format!("/relay/v1/admin/users/{}/{action}", f.user),
                &f.token,
                serde_json::json!({})
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
    assert!(f.store.gateway_auth(&f.credential).is_none());
    let c = f.store.0.lock().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT account FROM gateways WHERE id=?1",
            [f.gateway.to_string()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        f.user.to_string()
    );
    assert!(
        c.query_row(
            "SELECT must_change_password FROM accounts WHERE id=?1",
            [f.user.to_string()],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}

#[tokio::test]
async fn temporary_account_can_only_change_password_then_login_again() {
    let f = Fixture::new();
    let response = f
        .request(
            "POST",
            "/relay/v1/admin/users",
            &f.token,
            serde_json::json!({"username":"alice","temporaryPassword":"summer maple ninety 543"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let account = json(response).await["accountId"]
        .as_str()
        .unwrap()
        .to_string();
    let response = f
        .request(
            "POST",
            "/relay/v1/login",
            "",
            serde_json::json!({"username":"alice","password":"summer maple ninety 543"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    let token = cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string();
    assert_eq!(
        f.request("GET", "/relay/v1/gateways", &token, serde_json::json!({}))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let detail = json(
        f.request("GET", "/relay/v1/account", &token, serde_json::json!({}))
            .await,
    )
    .await;
    assert_eq!(detail["mustChangePassword"], true);
    assert_eq!(detail["role"], "user");
    let response=f.request("POST","/relay/v1/password",&token,serde_json::json!({"oldPassword":"summer maple ninety 543","password":"violet winter river 952"})).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(f.store.session(&token, None).is_none());
    assert!(
        !f.store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT must_change_password FROM accounts WHERE id=?1",
                [account],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
    let response = f
        .request(
            "POST",
            "/relay/v1/login",
            "",
            serde_json::json!({"username":"alice","password":"violet winter river 952"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    for path in ["/relay/v1/register", "/relay/v1/reset"] {
        assert_eq!(
            f.request(
                "POST",
                path,
                "",
                serde_json::json!({"token":"old","password":"violet winter river 952"})
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }
}

#[test]
fn transaction_authorization_rejects_role_or_generation_changed_during_hashing() {
    let f = Fixture::new();
    let session = f.store.session(&f.token, None).unwrap();
    let c = f.store.0.lock().unwrap();
    assert!(authorize(&c, &session).is_ok());
    c.execute(
        "UPDATE accounts SET role='user' WHERE id=?1",
        [f.admin.to_string()],
    )
    .unwrap();
    assert!(authorize(&c, &session).is_err());
    c.execute(
        "UPDATE accounts SET role='admin',auth_generation=auth_generation+1 WHERE id=?1",
        [f.admin.to_string()],
    )
    .unwrap();
    assert!(authorize(&c, &session).is_err());
}

#[tokio::test]
async fn account_limit_is_explicit_and_list_exposes_no_password_material() {
    let f = Fixture::new();
    {
        let mut c = f.store.0.lock().unwrap();
        let tx = c.transaction().unwrap();
        for index in 1..1000 {
            tx.execute("INSERT INTO accounts(id,username,password) VALUES(?1,?2,'synthetic-hash-not-for-display')",params![Uuid::new_v4().to_string(),format!("fixture_{index}")]).unwrap();
        }
        tx.commit().unwrap();
    }
    let response=f.request("POST","/relay/v1/admin/users",&f.token,serde_json::json!({"username":"over_limit","temporaryPassword":"summer maple ninety 543"})).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(json(response).await["error"], "account_limit");
    let response = f
        .request(
            "GET",
            "/relay/v1/admin/users",
            &f.token,
            serde_json::json!({}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("synthetic-hash-not-for-display"));
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["users"].as_array().unwrap().len(), 1000);
}

#[tokio::test]
async fn login_verified_before_disable_enable_cannot_mint_a_new_generation_session() {
    let f = Fixture::new();
    let password = "synthetic frozen login phrase 2873";
    let encoded = auth::hash_password_sync(password).unwrap();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET username='race_user',password=?1 WHERE id=?2",
            params![encoded, f.user.to_string()],
        )
        .unwrap();
    let gate = std::sync::Arc::new(auth::LoginGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    auth::LOGIN_GATE
        .lock()
        .unwrap()
        .insert("race_user".to_owned(), gate.clone());
    let router = f.router.clone();
    let login = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/relay/v1/login")
                    .header("host", "localhost:8123")
                    .header("origin", "http://localhost:8123")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username":"race_user","password":password}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), gate.entered.notified())
        .await
        .unwrap();
    for action in ["disable", "enable"] {
        assert_eq!(
            f.request(
                "POST",
                &format!("/relay/v1/admin/users/{}/{action}", f.user),
                &f.token,
                serde_json::json!({})
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
    gate.release.notify_one();
    let response = login.await.unwrap();
    auth::LOGIN_GATE.lock().unwrap().remove("race_user");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(
        f.store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE account=?1",
                [f.user.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn successful_login_replaces_only_present_browser_parent_and_its_grants() {
    let f = Fixture::new();
    let password = "synthetic switching cloud phrase 8741";
    let encoded = auth::hash_password_sync(password).unwrap();
    let old = f.store.session(&f.user_token, None).unwrap();
    let old_grant = Uuid::new_v4();
    let other_parent = Uuid::new_v4();
    let other_grant = Uuid::new_v4();
    let other_token = secret();
    {
        let c = f.store.0.lock().unwrap();
        c.execute(
            "UPDATE accounts SET password=?1 WHERE id=?2",
            params![encoded, f.admin.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                other_parent.to_string(),
                digest(&other_token),
                f.user.to_string(),
                now() + 3600
            ],
        )
        .unwrap();
        for (grant, parent) in [(old_grant, old.id), (other_grant, other_parent)] {
            c.execute("INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,?2,?3,?4,?5,?6)",params![grant.to_string(),digest(&secret()),f.user.to_string(),f.gateway.to_string(),parent.to_string(),now()+3600]).unwrap();
        }
    }
    assert!(f.store.active(old_grant));
    assert!(f.store.active(other_grant));
    let failed=f.request("POST","/relay/v1/login",&f.user_token,serde_json::json!({"username":"operator","password":"synthetic wrong cloud phrase 8741"})).await;
    assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);
    assert!(f.store.active(old_grant));
    let response = f
        .request(
            "POST",
            "/relay/v1/login",
            &f.user_token,
            serde_json::json!({"username":"operator","password":password}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let token = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    assert_eq!(f.store.session(token, None).unwrap().account, f.admin);
    assert!(!f.store.active(old_grant));
    assert!(f.store.session(&f.user_token, None).is_none());
    assert!(f.store.active(other_grant));
    assert!(f.store.session(&other_token, None).is_some());
}

#[tokio::test]
async fn concurrent_login_cannot_replace_an_already_consumed_browser_parent() {
    let f = Fixture::new();
    let password = "synthetic concurrent lake phrase 3962";
    let encoded = auth::hash_password_sync(password).unwrap();
    let second = Uuid::new_v4();
    {
        let c = f.store.0.lock().unwrap();
        c.execute(
            "UPDATE accounts SET password=?1 WHERE id=?2",
            params![encoded, f.admin.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO accounts(id,username,password) VALUES(?1,'race_second',?2)",
            params![second.to_string(), encoded],
        )
        .unwrap();
    }
    let gate = std::sync::Arc::new(auth::LoginGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    auth::LOGIN_GATE
        .lock()
        .unwrap()
        .insert("race_second".to_owned(), gate.clone());
    let router = f.router.clone();
    let cookie = format!("__Host-relay_session={}", f.user_token);
    let paused = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/relay/v1/login")
                    .header("host", "localhost:8123")
                    .header("origin", "http://localhost:8123")
                    .header("content-type", "application/json")
                    .header("cookie", cookie)
                    .body(Body::from(
                        serde_json::json!({"username":"race_second","password":password})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), gate.entered.notified())
        .await
        .unwrap();
    let first = f
        .request(
            "POST",
            "/relay/v1/login",
            &f.user_token,
            serde_json::json!({"username":"operator","password":password}),
        )
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    let token = first.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_owned();
    let parent = f.store.session(&token, None).unwrap();
    let grant = Uuid::new_v4();
    let gateway = Uuid::new_v4();
    {
        let c = f.store.0.lock().unwrap();
        c.execute("INSERT INTO gateways(id,account,name,credential) VALUES(?1,?2,'first login gateway',?3)",params![gateway.to_string(),f.admin.to_string(),digest(&secret())]).unwrap();
        c.execute("INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,?2,?3,?4,?5,?6)",params![grant.to_string(),digest(&secret()),f.admin.to_string(),gateway.to_string(),parent.id.to_string(),now()+3600]).unwrap();
    }
    gate.release.notify_one();
    let second_response = paused.await.unwrap();
    auth::LOGIN_GATE.lock().unwrap().remove("race_second");
    assert_eq!(second_response.status(), StatusCode::UNAUTHORIZED);
    assert!(second_response.headers().get("set-cookie").is_none());
    assert!(f.store.active(grant));
    assert!(f.store.session(&token, None).is_some());
    assert_eq!(
        f.store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE account=?1",
                [second.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
