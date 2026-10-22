use super::*;
use axum::body::Body;
use axum::http::{Request, header};
use codex_relay_protocol::{
    BindingPending, BindingPoll, BindingStart, Bound, Hello, ToGateway, VERSION,
    WORKBENCH_API_VERSION, Welcome,
};
use futures_util::{SinkExt, StreamExt};
use rusqlite::params;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tower::ServiceExt;

#[test]
fn portal_privacy_copy_matches_encrypted_channels_and_trusted_frontend() {
    let page = include_str!("../static/index.html");
    assert!(page.contains("浏览器与电脑间加密传输"));
    assert!(page.contains("前端及授权逻辑仍须可信"));
    assert!(!page.contains("公网服务可读取经它传输的内容"));
}

struct Fixture {
    _dir: tempfile::TempDir,
    config: Config,
    store: Store,
    account: Uuid,
    other: Uuid,
    gateway: Uuid,
    parent: Uuid,
    portal: String,
    browser: String,
    credential: String,
}
impl Fixture {
    fn new(port: u16) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            portal_origin: format!("http://localhost:{port}"),
            single_origin: true,
            password_workers: 4,
            allow_insecure_loopback: true,
            trusted_client_ip_header: None,
        };
        let store = Store::open(dir.path()).unwrap();
        let account = Uuid::new_v4();
        let other = Uuid::new_v4();
        let gateway = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let portal = store::secret();
        let browser = store::secret();
        let credential = store::secret();
        {
            let c = store.0.lock().unwrap();
            for (id, name) in [(account, "alice"), (other, "bob")] {
                c.execute(
                    "INSERT INTO accounts(id,username) VALUES(?1,?2)",
                    params![id.to_string(), name],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO gateways(id,account,name,credential,workbench_api_version) VALUES(?1,?2,'Synthetic workbench',?3,?4)",
                params![
                    gateway.to_string(),
                    account.to_string(),
                    store::digest(&credential),
                    WORKBENCH_API_VERSION
                ],
            )
            .unwrap();
            c.execute(
                "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
                params![
                    parent.to_string(),
                    store::digest(&portal),
                    account.to_string(),
                    store::now() + 3600
                ],
            )
            .unwrap();
            c.execute(
                "INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    Uuid::new_v4().to_string(),
                    store::digest(&browser),
                    account.to_string(),
                    gateway.to_string(),
                    parent.to_string(),
                    store::now() + 3600
                ],
            )
            .unwrap();
        }
        Self {
            _dir: dir,
            config,
            store,
            account,
            other,
            gateway,
            parent,
            portal,
            browser,
            credential,
        }
    }
    fn host(&self, gateway: bool) -> String {
        url::Url::parse(&if gateway {
            self.config.gateway_origin(self.gateway).unwrap()
        } else {
            self.config.portal_origin.clone()
        })
        .unwrap()
        .authority()
        .to_string()
    }
    fn request(
        &self,
        path: &str,
        method: &str,
        body: serde_json::Value,
        gateway: bool,
    ) -> Request<Body> {
        Request::builder()
            .uri(path)
            .method(method)
            .header("host", self.host(gateway))
            .header(
                "origin",
                if gateway {
                    self.config.gateway_origin(self.gateway).unwrap()
                } else {
                    self.config.portal_origin.clone()
                },
            )
            .header("content-type", "application/json")
            .header(
                "cookie",
                if gateway {
                    format!("__Host-workbench_session={}", self.browser)
                } else {
                    format!("__Host-relay_session={}", self.portal)
                },
            )
            .body(Body::from(body.to_string()))
            .unwrap()
    }
}
async fn json(response: Response) -> serde_json::Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

fn single_fixture(port: u16) -> Fixture {
    Fixture::new(port)
}
#[tokio::test]
async fn forget_rechecks_authorization_after_waiting_for_admission() {
    for mutation in [
        "DELETE FROM sessions WHERE account=?1",
        "UPDATE accounts SET auth_generation=auth_generation+1 WHERE id=?1",
        "UPDATE accounts SET disabled=1 WHERE id=?1",
        "UPDATE accounts SET must_change_password=1 WHERE id=?1",
    ] {
        let f = Fixture::new(9804);
        let state = synthetic_state(&f);
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *state.0.forget_gate.lock().unwrap() = Some(portal::ForgetGate {
            reached: reached.clone(),
            resume: resume.clone(),
        });
        let headers = f
            .request("/", "GET", serde_json::json!({}), false)
            .headers()
            .clone();
        let pending = tokio::spawn({
            let state = state.clone();
            let gateway = f.gateway;
            async move { portal::forget(State(state), axum::extract::Path(gateway), headers).await }
        });
        tokio::time::timeout(Duration::from_secs(2), reached.notified())
            .await
            .unwrap();
        state
            .0
            .store
            .0
            .lock()
            .unwrap()
            .execute(mutation, [f.account.to_string()])
            .unwrap();
        resume.notify_one();
        assert!(
            matches!(
                pending.await.unwrap(),
                Err(Failure(StatusCode::UNAUTHORIZED))
            ),
            "{mutation}"
        );
        let retained: bool = state
            .0
            .store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM gateways WHERE id=?1)",
                [f.gateway.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(retained);
    }
}
async fn mint_grant(f: &Fixture, router: &Router) -> Uuid {
    let response = router
        .clone()
        .oneshot(f.request(
            &format!("/relay/v1/gateways/{}/open", f.gateway),
            "POST",
            serde_json::json!({"uiApiVersion":WORKBENCH_API_VERSION}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    let url = url::Url::parse(body["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.origin().ascii_serialization(), f.config.portal_origin);
    assert_eq!(url.path(), format!("/g/{}/", f.gateway));
    let values = url::form_urlencoded::parse(url.fragment().unwrap().as_bytes())
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(values["session"], f.parent.to_string());
    assert_eq!(values["api"], WORKBENCH_API_VERSION.to_string());
    Uuid::parse_str(&values["grant"]).unwrap()
}
fn grant_request(f: &Fixture, grant: Uuid, _tail: &str) -> Request<Body> {
    let mut request = f.request(
        &format!("/relay/v1/gateways/{}/access", f.gateway),
        "GET",
        serde_json::json!({}),
        false,
    );
    request
        .headers_mut()
        .insert("x-workbench-grant", grant.to_string().parse().unwrap());
    request
        .headers_mut()
        .insert("x-workbench-api-version", WORKBENCH_API_VERSION.into());
    request
}
#[tokio::test]
async fn page_grant_requires_its_exact_parent_cookie_and_cannot_cross_accounts_or_gateways() {
    let f = single_fixture(9801);
    let router = connected_router(&f);
    let grant = mint_grant(&f, &router).await;
    assert_eq!(
        router
            .clone()
            .oneshot(grant_request(&f, grant, "application"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let second = store::secret();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                Uuid::new_v4().to_string(),
                store::digest(&second),
                f.account.to_string(),
                store::now() + 3600
            ],
        )
        .unwrap();
    for cookie in [
        format!("__Host-relay_session={second}"),
        format!("__Host-workbench_session={}", f.browser),
        String::new(),
    ] {
        let mut request = grant_request(&f, grant, "application");
        request
            .headers_mut()
            .insert("cookie", cookie.parse().unwrap());
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let mut request = grant_request(&f, grant, "application");
    *request.uri_mut() = format!("/relay/v1/gateways/{}/access", Uuid::new_v4())
        .parse()
        .unwrap();
    assert_eq!(
        router.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut request = grant_request(&f, grant, "application");
    request
        .headers_mut()
        .insert("origin", "https://evil.test".parse().unwrap());
    assert_eq!(
        router.clone().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let mut request = grant_request(&f, grant, "application");
    request
        .headers_mut()
        .insert("x-relay-account", "forged".parse().unwrap());
    assert_eq!(
        router.clone().oneshot(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET auth_generation=auth_generation+1 WHERE id=?1",
            [f.account.to_string()],
        )
        .unwrap();
    assert_eq!(
        router
            .oneshot(grant_request(&f, grant, "application"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn single_origin_never_proxies_documents_and_rejects_incompatible_opens() {
    let f = single_fixture(9802);
    let router = connected_router(&f);
    let grant = mint_grant(&f, &router).await;
    let page = router
        .clone()
        .oneshot(f.request(
            &format!("/g/{}/", f.gateway),
            "GET",
            serde_json::json!({}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(
        page.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("worker-src 'none'")
    );
    for path in [
        format!("/g/{}/assets/local.js", f.gateway),
        format!("/g/{}/workbench/v1/../application", f.gateway),
        format!("/g/{}/workbench/v1/%61pplication", f.gateway),
        format!("/g/{}/workbench/v1//application", f.gateway),
        format!("/g/{}/g/{}/workbench/v1/application", f.gateway, f.gateway),
        "/workbench/v1/application".into(),
    ] {
        let mut request = grant_request(&f, grant, "application");
        *request.uri_mut() = path.parse().unwrap();
        let status = router.clone().oneshot(request).await.unwrap().status();
        assert!(
            matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND),
            "{path}: {status}"
        );
    }
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE gateways SET workbench_api_version=99 WHERE id=?1",
            [f.gateway.to_string()],
        )
        .unwrap();
    let response = router
        .clone()
        .oneshot(f.request(
            &format!("/relay/v1/gateways/{}/open", f.gateway),
            "POST",
            serde_json::json!({"uiApiVersion":WORKBENCH_API_VERSION}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        router
            .clone()
            .oneshot(grant_request(&f, grant, "application"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let list = json(
        router
            .oneshot(f.request("/relay/v1/gateways", "GET", serde_json::json!({}), false))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(list["gateways"][0]["versionIncompatible"], true);
    assert_eq!(list["gateways"][0]["status"], "versionIncompatible");
}

#[test]
fn selectors_are_consumed_once_without_mutating_business_query() {
    let grant = Uuid::new_v4();
    let encoded_api: String = WORKBENCH_API_VERSION
        .to_string()
        .bytes()
        .map(|byte| format!("%{byte:02X}"))
        .collect();
    let headers = HeaderMap::new();
    let (id, query) = routing::selectors(
        &headers,
        Some(&format!(
            "relayGrant={grant}&path=a%2Fb&relayApi={WORKBENCH_API_VERSION}&epoch=abc"
        )),
        true,
    )
    .ok()
    .unwrap();
    assert_eq!(id, grant);
    assert_eq!(query, "path=a%2Fb&epoch=abc");
    for query in [
        format!("relayGrant={grant}&relayGrant={grant}&relayApi={WORKBENCH_API_VERSION}"),
        format!(
            "relayGrant={grant}&relayApi={WORKBENCH_API_VERSION}&relayApi={WORKBENCH_API_VERSION}"
        ),
        format!("relay%47rant={grant}&relayApi={WORKBENCH_API_VERSION}"),
        format!("relayGrant={grant}&relayApi={encoded_api}"),
        format!("relayGrant={grant}&relayApi=1"),
        format!("relayGrant={grant}&relayApi=99"),
        format!(
            "relayGrant={}&relayApi={WORKBENCH_API_VERSION}",
            grant.simple()
        ),
    ] {
        assert!(
            routing::selectors(&headers, Some(&query), true).is_err(),
            "{query}"
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert("x-workbench-grant", grant.to_string().parse().unwrap());
    headers.insert("x-workbench-api-version", WORKBENCH_API_VERSION.into());
    assert!(routing::selectors(&headers, None, false).is_ok());
    assert!(
        routing::selectors(
            &headers,
            Some(&format!(
                "relayGrant={grant}&relayApi={WORKBENCH_API_VERSION}"
            )),
            true
        )
        .is_err()
    );
    headers.append("x-workbench-grant", grant.to_string().parse().unwrap());
    assert!(routing::selectors(&headers, None, false).is_err());
}

#[tokio::test]
async fn reauthorization_requires_retained_machine_proof_and_current_exact_owner() {
    let mut f = single_fixture(9803);
    let router = app(f.config.clone()).unwrap();
    // Simulate the atomic password revocation, then a fresh owner's login.
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET auth_generation=2 WHERE id=?1",
            [f.account.to_string()],
        )
        .unwrap();
    assert!(f.store.gateway_auth(&f.credential).is_none());
    f.parent = Uuid::new_v4();
    f.portal = store::secret();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,hash,account,expires,auth_generation) VALUES(?1,?2,?3,?4,2)",
            params![
                f.parent.to_string(),
                store::digest(&f.portal),
                f.account.to_string(),
                store::now() + 3600
            ],
        )
        .unwrap();
    for value in [
        serde_json::json!({"name":"Reauthorize","resumeGatewayId":f.gateway}),
        serde_json::json!({"name":"Reauthorize","resumeGatewayId":f.gateway,"resumeCredential":"forged"}),
    ] {
        assert!(
            !router
                .clone()
                .oneshot(f.request("/relay/v1/bindings", "POST", value, false))
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    let start = serde_json::json!({"name":"Reauthorize","resumeGatewayId":f.gateway,"resumeCredential":f.credential});
    let pending: BindingPending = serde_json::from_value(
        json(
            router
                .clone()
                .oneshot(f.request("/relay/v1/bindings", "POST", start, false))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let other = store::secret();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                Uuid::new_v4().to_string(),
                store::digest(&other),
                f.other.to_string(),
                store::now() + 3600
            ],
        )
        .unwrap();
    for endpoint in ["preview", "approve"] {
        let mut request = f.request(
            &format!("/relay/v1/bindings/{endpoint}"),
            "POST",
            serde_json::json!({"userCode":pending.user_code}),
            false,
        );
        request.headers_mut().insert(
            "cookie",
            format!("__Host-relay_session={other}").parse().unwrap(),
        );
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        router
            .clone()
            .oneshot(f.request(
                "/relay/v1/bindings/approve",
                "POST",
                serde_json::json!({"userCode":pending.user_code}),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let bound: Bound = serde_json::from_value(
        json(
            router
                .clone()
                .oneshot(f.request(
                    &format!("/relay/v1/bindings/{}/poll", pending.binding_id),
                    "POST",
                    serde_json::json!({"deviceCode":pending.device_code}),
                    false,
                ))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    assert_eq!(bound.gateway_id, f.gateway);
    assert_eq!(bound.username, "alice");
    assert_ne!(bound.credential, f.credential);
    assert_eq!(
        f.store.gateway_auth(&bound.credential),
        Some((f.gateway, f.account))
    );
    assert!(f.store.gateway_auth(&f.credential).is_none());
    let pending: BindingPending=serde_json::from_value(json(router.clone().oneshot(f.request("/relay/v1/bindings","POST",serde_json::json!({"name":"Again","resumeGatewayId":f.gateway,"resumeCredential":bound.credential}),false)).await.unwrap()).await).unwrap();
    assert_eq!(
        router
            .clone()
            .oneshot(f.request(
                "/relay/v1/bindings/approve",
                "POST",
                serde_json::json!({"userCode":pending.user_code}),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET auth_generation=3 WHERE id=?1",
            [f.account.to_string()],
        )
        .unwrap();
    assert_eq!(
        router
            .oneshot(f.request(
                &format!("/relay/v1/bindings/{}/poll", pending.binding_id),
                "POST",
                serde_json::json!({"deviceCode":pending.device_code}),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn ownership_origin_and_legacy_exchange_fail_closed() {
    let f = Fixture::new(9876);
    let app = connected_router(&f);
    let path = format!("/relay/v1/gateways/{}/open", f.gateway);
    let mut req = f.request(
        &path,
        "POST",
        serde_json::json!({"uiApiVersion": WORKBENCH_API_VERSION}),
        false,
    );
    req.headers_mut()
        .insert("origin", "https://evil.test".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let other_cookie = store::secret();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                Uuid::new_v4().to_string(),
                store::digest(&other_cookie),
                f.other.to_string(),
                store::now() + 3600
            ],
        )
        .unwrap();
    let mut req = f.request(
        &path,
        "POST",
        serde_json::json!({"uiApiVersion":WORKBENCH_API_VERSION}),
        false,
    );
    req.headers_mut().insert(
        "cookie",
        format!("__Host-relay_session={other_cookie}")
            .parse()
            .unwrap(),
    );
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let grant = mint_grant(&f, &app).await;
    let before: i64 = f
        .store
        .0
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    let metadata = json(
        app.clone()
            .oneshot(grant_request(&f, grant, ""))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(metadata["grantId"], grant.to_string());
    assert_eq!(metadata["parentSessionId"], f.parent.to_string());
    assert_eq!(metadata["accountId"], f.account.to_string());
    assert_eq!(metadata.as_object().unwrap().len(), 8);
    let after: i64 = f
        .store
        .0
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "access must not mint grants");
    for path in [
        "/relay/v1/session/exchange",
        "/relay/v1/open",
        "/relay/v1/exchange.js",
        "/workbench/v1/application",
    ] {
        let response = app
            .clone()
            .oneshot(f.request(
                path,
                if path.ends_with("exchange") {
                    "POST"
                } else {
                    "GET"
                },
                serde_json::json!({"code":"unused"}),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        app.clone()
            .oneshot(f.request("/relay/v1/logout", "POST", serde_json::json!({}), false))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert!(!f.store.active(grant));
    assert!(!f.store.active(f.parent));
}
#[tokio::test]
async fn binding_is_approved_and_credential_is_one_use() {
    let f = Fixture::new(9877);
    let app = app(f.config.clone()).unwrap();
    let pending: BindingPending = serde_json::from_value(
        json(
            app.clone()
                .oneshot(
                    f.request(
                        "/relay/v1/bindings",
                        "POST",
                        serde_json::to_value(BindingStart {
                            name: "My synthetic computer".into(),
                            resume_gateway_id: None,
                            resume_credential: None,
                        })
                        .unwrap(),
                        false,
                    ),
                )
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let path = format!("/relay/v1/bindings/{}/poll", pending.binding_id);
    assert_eq!(
        app.clone()
            .oneshot(f.request(
                &path,
                "POST",
                serde_json::json!({"deviceCode":"bad"}),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let poll = || {
        f.request(
            &path,
            "POST",
            serde_json::to_value(BindingPoll {
                device_code: pending.device_code.clone(),
            })
            .unwrap(),
            false,
        )
    };
    assert_eq!(
        app.clone().oneshot(poll()).await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        app.clone()
            .oneshot(f.request(
                "/relay/v1/bindings/approve",
                "POST",
                serde_json::json!({"userCode":pending.user_code}),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let bound: Bound =
        serde_json::from_value(json(app.clone().oneshot(poll()).await.unwrap()).await).unwrap();
    assert_eq!(bound.account_id, f.account);
    assert_eq!(
        f.store.gateway_auth(&bound.credential),
        Some((bound.gateway_id, f.account))
    );
    assert_eq!(
        app.clone().oneshot(poll()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(f.request(
                &format!("/relay/v1/gateways/{}", bound.gateway_id),
                "DELETE",
                serde_json::json!(null),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert!(f.store.gateway_auth(&bound.credential).is_none());
}
#[tokio::test]
async fn credential_revoke_needs_its_own_credential_and_cascades_browser_sessions() {
    let f = Fixture::new(9986);
    let router = app(f.config.clone()).unwrap();
    let request = || {
        f.request(
            "/relay/v1/gateway/revoke",
            "POST",
            serde_json::json!({}),
            false,
        )
    };
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut bad = request();
    bad.headers_mut()
        .insert("authorization", "Bearer invalid".parse().unwrap());
    assert_eq!(
        router.clone().oneshot(bad).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(f.store.gateway_auth(&f.credential).is_some());
    let mut valid = request();
    valid.headers_mut().remove("origin");
    valid.headers_mut().remove("cookie");
    valid.headers_mut().insert(
        "authorization",
        format!("Bearer {}", f.credential).parse().unwrap(),
    );
    assert_eq!(
        router.oneshot(valid).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert!(f.store.gateway_auth(&f.credential).is_none());
    assert!(f.store.session(&f.browser, Some(f.gateway)).is_none());
    assert!(f.store.session(&f.portal, None).is_some());
}

#[tokio::test]
async fn administrator_provisions_resets_and_disables_account() {
    let f = Fixture::new(9878);
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE accounts SET role='admin' WHERE id=?1",
            [f.account.to_string()],
        )
        .unwrap();
    let app = app(f.config.clone()).unwrap();
    assert!(!auth::valid_password("passwordpassword"));
    assert!(!auth::valid_password("short"));
    let password = "A unique synthetic phrase 9482";
    let register = || {
        f.request(
            "/relay/v1/admin/users",
            "POST",
            serde_json::json!({"username":"charlie","temporaryPassword":password}),
            false,
        )
    };
    let created = app.clone().oneshot(register()).await.unwrap();
    assert!(created.status().is_success());
    let id: String = f
        .store
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT id FROM accounts WHERE username='charlie'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        app.clone().oneshot(register()).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    let login = || {
        let mut request = f.request(
            "/relay/v1/login",
            "POST",
            serde_json::json!({"username":"charlie","password":password}),
            false,
        );
        // The user signs in from another browser; account switching in the
        // administrator's browser intentionally revokes its original session.
        request.headers_mut().remove(header::COOKIE);
        request
    };
    let response = app.clone().oneshot(login()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let token = cookie.split('=').nth(1).unwrap();
    assert!(f.store.session(token, None).is_some());
    assert!(
        app.clone()
            .oneshot(f.request(
                &format!("/relay/v1/admin/users/{id}/password"),
                "POST",
                serde_json::json!({"temporaryPassword":"Another synthetic phrase 15792"}),
                false
            ))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(f.store.session(token, None).is_none());
    assert_eq!(
        app.clone().oneshot(login()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    f.store.disable("charlie").unwrap();
    assert_eq!(
        app.oneshot(login()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

async fn socket(
    port: u16,
    host: &str,
    path: &str,
    credential: Option<&str>,
    cookie: Option<&str>,
    origin: Option<&str>,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut request = format!("ws://127.0.0.1:{port}{path}")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert("host", host.parse().unwrap());
    if let Some(c) = credential {
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {c}").parse().unwrap());
    }
    if let Some(c) = cookie {
        request.headers_mut().insert("cookie", c.parse().unwrap());
    }
    if let Some(o) = origin {
        request.headers_mut().insert("origin", o.parse().unwrap());
    }
    connect_async(request).await.unwrap().0
}
#[tokio::test]
async fn legacy_hello_zero_is_incompatible_even_after_disconnect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = single_fixture(port);
    let router = app(f.config.clone()).unwrap();
    let server = tokio::spawn({
        let router = router.clone();
        async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        }
    });
    let mut control = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    control
        .send(Message::Text(
            serde_json::to_string(&Hello {
                version: VERSION,
                workbench_api_version: 0,
                gateway_id: f.gateway,
                instance_id: Uuid::new_v4(),
                client_version: "legacy-fixture".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let _: Welcome =
        serde_json::from_str(control.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    let stored: u32 = f
        .store
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT workbench_api_version FROM gateways WHERE id=?1",
            [f.gateway.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, u32::MAX, "zero remains reserved for no Hello yet");
    for disconnected in [false, true] {
        if disconnected {
            control.close(None).await.unwrap();
        }
        let list = json(
            router
                .clone()
                .oneshot(f.request("/relay/v1/gateways", "GET", serde_json::json!({}), false))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(list["gateways"][0]["status"], "versionIncompatible");
        assert_eq!(list["gateways"][0]["online"], false);
        let response = router
            .clone()
            .oneshot(f.request(
                &format!("/relay/v1/gateways/{}/open", f.gateway),
                "POST",
                serde_json::json!({"uiApiVersion": WORKBENCH_API_VERSION}),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    server.abort();
    let _ = server.await;
}
type TestSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn hello(control: &mut TestSocket, f: &Fixture) -> Welcome {
    control
        .send(Message::Text(
            serde_json::to_string(&Hello {
                version: VERSION,
                workbench_api_version: WORKBENCH_API_VERSION,
                gateway_id: f.gateway,
                instance_id: Uuid::new_v4(),
                client_version: "synthetic".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    serde_json::from_str(control.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
}

async fn channel_pair(
    control: &mut TestSocket,
    f: &Fixture,
    port: u16,
    grant: Uuid,
    scope: &str,
) -> (
    TestSocket,
    TestSocket,
    codex_relay_protocol::secure::ChannelBinding,
) {
    let browser = tokio::spawn({
        let host = f.host(false);
        let origin = f.config.portal_origin.clone();
        let cookie = format!("__Host-relay_session={}", f.portal);
        let path = format!(
            "/g/{}/workbench/v1/secure-channel?relayGrant={grant}&relayApi={WORKBENCH_API_VERSION}&{scope}",
            f.gateway
        );
        async move { socket(port, &host, &path, None, Some(&cookie), Some(&origin)).await }
    });
    let binding = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = control.next().await.unwrap().unwrap();
            if let ToGateway::OpenChannel { binding } =
                serde_json::from_str(message.to_text().unwrap()).unwrap()
            {
                break binding;
            }
        }
    })
    .await
    .unwrap();
    let agent = socket(
        port,
        &f.host(false),
        &format!(
            "/relay/v1/tunnel/channels/{}?epoch={}",
            binding.channel_id, binding.context.connection_epoch
        ),
        Some(&f.credential),
        None,
        None,
    )
    .await;
    let mut public = browser.await.unwrap();
    let envelope: serde_json::Value =
        serde_json::from_str(public.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(envelope["type"], "channel_binding");
    assert_eq!(envelope["binding"], serde_json::to_value(&binding).unwrap());
    assert_eq!(binding.context.grant_id, grant);
    assert_eq!(binding.context.parent_session_id, f.parent);
    assert_eq!(binding.context.account_id, f.account);
    assert_eq!(binding.context.gateway_id, f.gateway);
    (public, agent, binding)
}

#[tokio::test]
async fn opaque_channels_preserve_bytes_and_logout_closes_both_scopes() {
    opaque_revocation(false).await;
}

#[tokio::test]
async fn account_deletion_closes_opaque_channels_and_control() {
    opaque_revocation(true).await;
}

async fn opaque_revocation(delete_account: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = Fixture::new(port);
    let router = app(f.config.clone()).unwrap();
    let server = tokio::spawn({
        let router = router.clone();
        async move {
            axum::serve(listener, router).await.unwrap();
        }
    });
    let mut control = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    let welcome = hello(&mut control, &f).await;
    let grant = mint_grant(&f, &router).await;
    let (mut app_public, mut app_agent, app_binding) =
        channel_pair(&mut control, &f, port, grant, "scope=application").await;
    let run = Uuid::new_v4();
    let (mut run_public, mut run_agent, run_binding) = channel_pair(
        &mut control,
        &f,
        port,
        grant,
        &format!("scope=run&runId={run}"),
    )
    .await;
    assert_eq!(
        run_binding.scope,
        codex_relay_protocol::secure::Scope::Run { run_id: run }
    );
    assert_ne!(app_binding.channel_id, run_binding.channel_id);
    assert_eq!(
        app_binding.context.connection_epoch,
        welcome.connection_epoch
    );
    // Relay must carry handshake bytes and arbitrary encrypted bytes unchanged.
    let hello_text = "{\"type\":\"client_hello\",\"publicKey\":\"synthetic opaque\"}";
    app_public
        .send(Message::Text(hello_text.into()))
        .await
        .unwrap();
    assert_eq!(
        app_agent.next().await.unwrap().unwrap().to_text().unwrap(),
        hello_text
    );
    let server_text = "{\"type\":\"server_hello\",\"publicKey\":\"synthetic opaque\"}";
    app_agent
        .send(Message::Text(server_text.into()))
        .await
        .unwrap();
    assert_eq!(
        app_public.next().await.unwrap().unwrap().to_text().unwrap(),
        server_text
    );
    for (public, agent) in [
        (&mut app_public, &mut app_agent),
        (&mut run_public, &mut run_agent),
    ] {
        let up = vec![0x91; 64 * 1024];
        let down = vec![0xa3; 64 * 1024];
        let (sent_up, sent_down) = tokio::join!(
            public.send(Message::Binary(up.clone().into())),
            agent.send(Message::Binary(down.clone().into()))
        );
        sent_up.unwrap();
        sent_down.unwrap();
        assert_eq!(
            agent.next().await.unwrap().unwrap().into_data().as_ref(),
            up
        );
        assert_eq!(
            public.next().await.unwrap().unwrap().into_data().as_ref(),
            down
        );
    }
    let (path, method, token, expected) = if delete_account {
        let token = store::secret();
        let c = f.store.0.lock().unwrap();
        c.execute(
            "UPDATE accounts SET role=\'admin\' WHERE id=?1",
            [f.other.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                Uuid::new_v4().to_string(),
                store::digest(&token),
                f.other.to_string(),
                store::now() + 3600
            ],
        )
        .unwrap();
        (
            format!("/relay/v1/admin/users/{}", f.account),
            "DELETE",
            token,
            StatusCode::NO_CONTENT,
        )
    } else {
        (
            "/relay/v1/logout".into(),
            "POST",
            f.portal.clone(),
            StatusCode::OK,
        )
    };
    let mut req = f.request(&path, method, serde_json::json!({}), false);
    req.headers_mut().insert(
        "cookie",
        format!("__Host-relay_session={token}").parse().unwrap(),
    );
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status(),
        expected
    );
    for socket in [
        &mut app_public,
        &mut app_agent,
        &mut run_public,
        &mut run_agent,
    ] {
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), socket.next()).await,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_))))
        ));
    }
    assert!(!f.store.active(grant));
    if delete_account {
        assert!(f.store.gateway_auth(&f.credential).is_none());
    } else {
        let mut replacement = socket(
            port,
            &f.host(false),
            "/relay/v1/tunnel",
            Some(&f.credential),
            None,
            None,
        )
        .await;
        let next = hello(&mut replacement, &f).await;
        assert_ne!(next.connection_epoch, welcome.connection_epoch);
        let mut req = format!(
            "ws://127.0.0.1:{port}/relay/v1/tunnel/channels/{}?epoch={}",
            app_binding.channel_id, welcome.connection_epoch
        )
        .into_client_request()
        .unwrap();
        req.headers_mut()
            .insert("host", f.host(false).parse().unwrap());
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", f.credential).parse().unwrap(),
        );
        assert!(connect_async(req).await.is_err());
    }
    server.abort();
}
fn secure_request(
    f: &Fixture,
    port: u16,
    grant: Uuid,
    extra: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut req=format!("ws://127.0.0.1:{port}/g/{}/workbench/v1/secure-channel?relayGrant={grant}&relayApi={WORKBENCH_API_VERSION}&{extra}",f.gateway).into_client_request().unwrap();
    req.headers_mut()
        .insert("host", f.host(false).parse().unwrap());
    req.headers_mut()
        .insert("origin", f.config.portal_origin.parse().unwrap());
    req.headers_mut().insert(
        "cookie",
        format!("__Host-relay_session={}", f.portal)
            .parse()
            .unwrap(),
    );
    req
}

async fn websocket_status(req: tokio_tungstenite::tungstenite::http::Request<()>) -> StatusCode {
    match connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status(),
        _ => panic!("expected rejected WebSocket handshake"),
    }
}

#[tokio::test]
async fn secure_admission_authenticates_before_offline_and_capacity_checks() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = Fixture::new(port);
    let grant = f.store.session(&f.browser, Some(f.gateway)).unwrap().id;
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE sessions SET ui_api_version=?1 WHERE id=?2",
            params![WORKBENCH_API_VERSION, grant.to_string()],
        )
        .unwrap();
    let state = synthetic_state(&f);
    let router = router(state.clone());
    let server = tokio::spawn({
        let router = router.clone();
        async move {
            axum::serve(listener, router).await.unwrap();
        }
    });
    assert_eq!(
        websocket_status(secure_request(&f, port, grant, "scope=application")).await,
        StatusCode::SERVICE_UNAVAILABLE,
        "a valid grant still distinguishes an offline gateway"
    );
    assert_eq!(
        router
            .oneshot(f.request("/relay/v1/logout", "POST", serde_json::json!({}), false))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert!(!f.store.active(f.parent));
    assert_eq!(
        websocket_status(secure_request(&f, port, grant, "scope=application")).await,
        StatusCode::UNAUTHORIZED,
        "offline state must not hide parent revocation"
    );
    let _occupied = state
        .0
        .stream_capacity
        .clone()
        .try_acquire_many_owned(256)
        .unwrap();
    assert_eq!(
        websocket_status(secure_request(&f, port, grant, "scope=application")).await,
        StatusCode::UNAUTHORIZED,
        "capacity exhaustion must not hide parent revocation"
    );
    assert!(state.0.streams.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn secure_admission_rejects_foreign_parent_selectors_scope_and_capacity_overflow() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = Fixture::new(port);
    let state = synthetic_state(&f);
    let router = router(state.clone());
    let server = tokio::spawn({
        let router = router.clone();
        async move {
            axum::serve(listener, router).await.unwrap();
        }
    });
    let mut control = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    hello(&mut control, &f).await;
    let grant = mint_grant(&f, &router).await;
    for scope in [
        "scope=run",
        "scope=application&runId=unexpected",
        "scope=application&path=private",
        "scope=application&scope=run",
        "scope=%61pplication",
        "scope=application&relayGrant=duplicate",
    ] {
        assert_eq!(
            websocket_status(secure_request(&f, port, grant, scope)).await,
            StatusCode::BAD_REQUEST,
            "{scope}"
        );
    }
    for same_account in [true, false] {
        let token = store::secret();
        f.store
            .0
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
                params![
                    Uuid::new_v4().to_string(),
                    store::digest(&token),
                    if same_account { f.account } else { f.other }.to_string(),
                    store::now() + 3600
                ],
            )
            .unwrap();
        let mut req = secure_request(&f, port, grant, "scope=application");
        req.headers_mut().insert(
            "cookie",
            format!("__Host-relay_session={token}").parse().unwrap(),
        );
        assert_eq!(websocket_status(req).await, StatusCode::UNAUTHORIZED);
    }
    let mut req = secure_request(&f, port, grant, "scope=application");
    req.headers_mut().remove("cookie");
    assert_eq!(websocket_status(req).await, StatusCode::UNAUTHORIZED);
    let mut req = secure_request(&f, port, grant, "scope=application");
    req.headers_mut()
        .insert("x-workbench-grant", grant.to_string().parse().unwrap());
    assert_eq!(websocket_status(req).await, StatusCode::BAD_REQUEST);
    let mut req = secure_request(&f, port, grant, "scope=application");
    req.headers_mut()
        .insert("origin", "https://evil.test".parse().unwrap());
    assert_eq!(websocket_status(req).await, StatusCode::FORBIDDEN);
    let per_gateway = state
        .0
        .tunnels
        .lock()
        .unwrap()
        .get(&f.gateway)
        .unwrap()
        .capacity
        .clone();
    assert_eq!(per_gateway.available_permits(), 64);
    assert_eq!(state.0.stream_capacity.available_permits(), 256);
    let occupied = per_gateway.clone().try_acquire_many_owned(64).unwrap();
    // A new control epoch must keep the physical permits held by old channels.
    let mut replacement = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    hello(&mut replacement, &f).await;
    assert_eq!(
        state
            .0
            .tunnels
            .lock()
            .unwrap()
            .get(&f.gateway)
            .unwrap()
            .capacity
            .available_permits(),
        0
    );
    control = replacement;
    assert_eq!(
        websocket_status(secure_request(&f, port, grant, "scope=application")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(occupied);
    let occupied = state
        .0
        .stream_capacity
        .clone()
        .try_acquire_many_owned(256)
        .unwrap();
    assert_eq!(
        websocket_status(secure_request(&f, port, grant, "scope=application")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(occupied);
    assert!(state.0.streams.lock().unwrap().is_empty());
    // All rejected attempts released capacity; a real channel can still open.
    let (public, agent, _) = channel_pair(&mut control, &f, port, grant, "scope=application").await;
    drop(public);
    drop(agent);
    server.abort();
}

#[tokio::test]
async fn oversized_records_and_plaintext_after_binary_close_only_their_channel() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = Fixture::new(port);
    let router = app(f.config.clone()).unwrap();
    let server = tokio::spawn({
        let router = router.clone();
        async move {
            axum::serve(listener, router).await.unwrap();
        }
    });
    let mut control = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    hello(&mut control, &f).await;
    let grant = mint_grant(&f, &router).await;
    for invalid in [
        Message::Text("x".repeat(16 * 1024 + 1).into()),
        Message::Binary(vec![0; 96 * 1024 + 1].into()),
        Message::Text("plaintext after finished".into()),
    ] {
        let (mut public, mut agent, _) =
            channel_pair(&mut control, &f, port, grant, "scope=application").await;
        if invalid == Message::Text("plaintext after finished".into()) {
            public
                .send(Message::Binary(vec![1, 2, 3].into()))
                .await
                .unwrap();
            assert_eq!(
                agent.next().await.unwrap().unwrap().into_data().as_ref(),
                [1, 2, 3]
            );
        }
        public.send(invalid).await.unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), agent.next()).await,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_))))
        ));
    }
    let (mut public, mut agent, _) =
        channel_pair(&mut control, &f, port, grant, "scope=application").await;
    agent
        .send(Message::Binary(vec![4, 5, 6].into()))
        .await
        .unwrap();
    assert_eq!(
        public.next().await.unwrap().unwrap().into_data().as_ref(),
        [4, 5, 6]
    );
    server.abort();
}

#[tokio::test]
async fn unfinished_handshake_times_out_and_releases_physical_channel_budget() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = Fixture::new(port);
    let state = synthetic_state(&f);
    let router = router(state.clone());
    let server = tokio::spawn({
        let router = router.clone();
        async move {
            axum::serve(listener, router).await.unwrap();
        }
    });
    let mut control = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    hello(&mut control, &f).await;
    let grant = mint_grant(&f, &router).await;
    let (mut public, _agent, _) =
        channel_pair(&mut control, &f, port, grant, "scope=application").await;
    assert_eq!(state.0.stream_capacity.available_permits(), 255);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(12), public.next()).await,
        Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_))))
    ));
    assert!(state.0.streams.lock().unwrap().is_empty());
    assert_eq!(state.0.stream_capacity.available_permits(), 256);
    assert_eq!(
        state
            .0
            .tunnels
            .lock()
            .unwrap()
            .get(&f.gateway)
            .unwrap()
            .capacity
            .available_permits(),
        64
    );
    server.abort();
}

#[tokio::test]
async fn opening_offline_gateway_does_not_create_page_grant() {
    let f = Fixture::new(9879);
    let router = app(f.config.clone()).unwrap();
    let count = || {
        f.store
            .0
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before = count();
    let response = router
        .oneshot(f.request(
            &format!("/relay/v1/gateways/{}/open", f.gateway),
            "POST",
            serde_json::json!({"uiApiVersion":WORKBENCH_API_VERSION}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(count(), before);
}

fn synthetic_state(f: &Fixture) -> App {
    App(Arc::new(Inner {
        config: f.config.clone(),
        store: Store::open(&f.config.data_dir).unwrap(),
        passwords: Arc::new(Semaphore::new(4)),
        portal_capacity: Arc::new(Semaphore::new(32)),
        control_capacity: Arc::new(Semaphore::new(256)),
        gateway_control_capacity: Mutex::new(HashMap::new()),
        gateway_channel_capacity: Mutex::new(HashMap::new()),
        stream_capacity: Arc::new(Semaphore::new(256)),

        limits: Mutex::new(HashMap::new()),
        bindings: Mutex::new(HashMap::new()),
        tunnels: Mutex::new(HashMap::new()),
        streams: Mutex::new(HashMap::new()),
        admission_gate: Mutex::new(None),
        forget_gate: Mutex::new(None),
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_account_cancels_control_and_fences_authenticated_reconnect() {
    let f = Fixture::new(9780);
    let s = synthetic_state(&f);
    let admin_token = store::secret();
    {
        let c = s.0.store.0.lock().unwrap();
        c.execute(
            "UPDATE accounts SET role='admin' WHERE id=?1",
            [f.other.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                Uuid::new_v4().to_string(),
                store::digest(&admin_token),
                f.other.to_string(),
                store::now() + 3600
            ],
        )
        .unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = Router::new()
        .route("/relay/v1/tunnel", get(tunnel::connect))
        .with_state(s.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let hello = || {
        Message::Text(
            serde_json::to_string(&Hello {
                version: VERSION,
                workbench_api_version: codex_relay_protocol::WORKBENCH_API_VERSION,
                gateway_id: f.gateway,
                instance_id: Uuid::new_v4(),
                client_version: "synthetic-delete-race".into(),
            })
            .unwrap()
            .into(),
        )
    };
    let mut active = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    active.send(hello()).await.unwrap();
    let welcome = tokio::time::timeout(Duration::from_secs(5), active.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let _: Welcome = serde_json::from_str(welcome.to_text().unwrap()).unwrap();
    let cancellation =
        s.0.tunnels
            .lock()
            .unwrap()
            .get(&f.gateway)
            .unwrap()
            .cancel
            .subscribe();
    let reached = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *s.0.admission_gate.lock().unwrap() = Some(tunnel::AdmissionGate {
        credential: f.credential.clone(),
        reached: reached.clone(),
        resume: resume.clone(),
    });
    let mut stale = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    stale.send(hello()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        format!("__Host-relay_session={admin_token}")
            .parse()
            .unwrap(),
    );
    assert_eq!(
        admin::delete_user(State(s.clone()), axum::extract::Path(f.account), headers)
            .await
            .ok(),
        Some(StatusCode::NO_CONTENT)
    );
    assert!(*cancellation.borrow());
    assert!(!s.0.tunnels.lock().unwrap().contains_key(&f.gateway));
    assert!(s.0.store.gateway_auth(&f.credential).is_none());
    assert!(s.0.store.session(&f.browser, Some(f.gateway)).is_none());
    resume.notify_one();
    let message = tokio::time::timeout(Duration::from_secs(5), stale.next())
        .await
        .unwrap();
    assert!(!matches!(
        message,
        Some(Ok(Message::Text(_) | Message::Binary(_)))
    ));
    assert!(!s.0.tunnels.lock().unwrap().contains_key(&f.gateway));
    server.abort();
}
#[tokio::test]
async fn trusted_proxy_ip_requires_opt_in_loopback_and_exactly_one_ip() {
    let mut f = Fixture::new(9780);
    f.config.trusted_client_ip_header = Some("x-relay-client-ip".into());
    let router = app(f.config.clone()).unwrap();
    for (peer, value, duplicate, expected) in [
        ("127.0.0.1:1234", "203.0.113.9", false, StatusCode::OK),
        (
            "192.0.2.2:1234",
            "203.0.113.9",
            false,
            StatusCode::BAD_REQUEST,
        ),
        (
            "127.0.0.1:1234",
            "203.0.113.9, 192.0.2.2",
            false,
            StatusCode::BAD_REQUEST,
        ),
        ("127.0.0.1:1234", "garbage", false, StatusCode::BAD_REQUEST),
        (
            "127.0.0.1:1234",
            "203.0.113.9",
            true,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut req = f.request("/relay/v1/account", "GET", serde_json::json!(null), false);
        req.extensions_mut().insert(axum::extract::ConnectInfo(
            peer.parse::<SocketAddr>().unwrap(),
        ));
        req.headers_mut()
            .insert("x-relay-client-ip", value.parse().unwrap());
        if duplicate {
            req.headers_mut()
                .append("x-relay-client-ip", "198.51.100.1".parse().unwrap());
        }
        assert_eq!(
            router.clone().oneshot(req).await.unwrap().status(),
            expected
        );
    }
    assert_eq!(
        router
            .oneshot(f.request("/relay/v1/account", "GET", serde_json::json!(null), false))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    f.config.trusted_client_ip_header = None;
    let state = synthetic_state(&f);
    let mut h = HeaderMap::new();
    h.insert("x-relay-client-ip", "203.0.113.9".parse().unwrap());
    assert_eq!(
        state
            .client_ip(&h, Some("127.0.0.1:1234".parse().unwrap()))
            .unwrap_or_else(|_| panic!("peer IP")),
        "127.0.0.1"
    );
}
#[tokio::test]
async fn idle_grants_remain_renewable_and_are_revoked_without_streams() {
    let f = Fixture::new(9782);
    let s = synthetic_state(&f);
    let session = f.store.session(&f.browser, Some(f.gateway)).unwrap();
    let (send, mut recv) = tokio::sync::mpsc::channel(128);
    let (cancel, _stop) = tokio::sync::watch::channel(false);
    s.0.tunnels.lock().unwrap().insert(
        f.gateway,
        tunnel::Connection {
            credential: f.credential.clone(),
            workbench_api_version: codex_relay_protocol::WORKBENCH_API_VERSION,
            epoch: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            sessions: std::collections::HashSet::from([session.id]),
            capacity: Arc::new(Semaphore::new(64)),

            send,
            cancel,
        },
    );
    s.revoke_invalid();
    assert!(
        s.0.tunnels
            .lock()
            .unwrap()
            .get(&f.gateway)
            .unwrap()
            .sessions
            .contains(&session.id)
    );
    assert!(recv.try_recv().is_err());
    f.store.disable("alice").unwrap();
    s.revoke_invalid();
    assert!(recv.recv().await.is_none());
    assert!(!s.0.tunnels.lock().unwrap().contains_key(&f.gateway));
}

#[cfg(unix)]
#[test]
fn private_storage_rejects_links_and_future_schema_preserves_audit() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("private");
    let store = Store::open(&data).unwrap();
    assert_eq!(
        std::fs::metadata(&data).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(data.join("relay.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let token = store.issue("synthetic_invitee", false).unwrap();
    let account: String = store
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT id FROM accounts WHERE username='synthetic_invitee'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let c = store.0.lock().unwrap();
    c.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10005) INSERT INTO audit(at,event) SELECT strftime('%s','now'),'synthetic' FROM n").unwrap();
    store::audit(&c, "test", Some(&account), None).unwrap();
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        10000
    );
    drop(c);
    drop(store);
    let reopened = Store::open(&data).unwrap();
    assert_eq!(
        reopened
            .0
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert!(
        reopened
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tokens WHERE hash=?1)",
                [store::digest(&token)],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
    reopened
        .0
        .lock()
        .unwrap()
        .execute_batch("PRAGMA user_version=4")
        .unwrap();
    drop(reopened);
    assert!(Store::open(&data).is_err());
    let target = dir.path().join("target");
    std::fs::write(&target, "untouched").unwrap();
    for file in [
        "relay.sqlite3",
        "relay.sqlite3-wal",
        "relay.sqlite3-shm",
        "relay.sqlite3-journal",
    ] {
        let root = dir.path().join(Uuid::new_v4().to_string());
        std::fs::create_dir(&root).unwrap();
        symlink(&target, root.join(file)).unwrap();
        assert!(Store::open(&root).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    }
    let linked = dir.path().join("root-link");
    symlink(&data, &linked).unwrap();
    assert!(Store::open(&linked).is_err());
    let hard = dir.path().join("hard");
    std::fs::create_dir(&hard).unwrap();
    std::fs::hard_link(&target, hard.join("relay.sqlite3")).unwrap();
    assert!(Store::open(&hard).is_err());
}
#[tokio::test]
async fn anonymous_json_bodies_have_small_size_and_concurrency_bounds() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = Fixture::new(9783);
    let router = app(f.config.clone()).unwrap();
    let mut too_large = f.request("/relay/v1/login", "POST", serde_json::json!({}), false);
    *too_large.body_mut() = Body::from(vec![b' '; 8193]);
    assert_eq!(
        router.clone().oneshot(too_large).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let started = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for i in 0..32 {
        let mut req = f.request(
            if i % 2 == 0 {
                "/relay/v1/login"
            } else {
                "/relay/v1/bindings"
            },
            "POST",
            serde_json::json!({}),
            false,
        );
        req.headers_mut().remove("cookie");
        let count = started.clone();
        let mut first = true;
        *req.body_mut() = Body::from_stream(futures_util::stream::poll_fn(move |_| {
            if first {
                first = false;
                count.fetch_add(1, Ordering::SeqCst);
            }
            std::task::Poll::<Option<std::result::Result<axum::body::Bytes,std::io::Error>>>::Pending
        }));
        let app = router.clone();
        tasks.push(tokio::spawn(async move { app.oneshot(req).await.unwrap() }));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while started.load(Ordering::SeqCst) < 32 {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(
        router
            .clone()
            .oneshot(f.request("/relay/v1/login", "POST", serde_json::json!({}), false))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    // Capacity is released when clients abandon incomplete bodies.
    assert_ne!(
        router
            .oneshot(f.request("/relay/v1/login", "POST", serde_json::json!({}), false))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}
#[tokio::test]
async fn control_handshake_admission_is_bounded_and_released() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = Fixture::new(port);
    let router = app(f.config.clone()).unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let one = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    let two = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    let request = || {
        let mut r = format!("ws://127.0.0.1:{port}/relay/v1/tunnel")
            .into_client_request()
            .unwrap();
        r.headers_mut()
            .insert("host", f.host(false).parse().unwrap());
        r.headers_mut().insert(
            "authorization",
            format!("Bearer {}", f.credential).parse().unwrap(),
        );
        r
    };
    let error = connect_async(request()).await.unwrap_err();
    assert!(matches!(error,tokio_tungstenite::tungstenite::Error::Http(r) if r.status()==429));
    drop(one);
    // Allow the server to consume EOF; retry only the synthetic admission check, never a user mutation.
    let mut recovered = false;
    for _ in 0..20 {
        if connect_async(request()).await.is_ok() {
            recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(recovered);
    drop(two);
    server.abort();
}

#[test]
fn expired_metadata_cleanup_preserves_valid_rows_and_bounds_each_batch() {
    let f = Fixture::new(9784);
    let time = store::now();
    let valid_parent = Uuid::new_v4();
    let valid_child = Uuid::new_v4();
    {
        let c = f.store.0.lock().unwrap();
        c.execute(
            "UPDATE sessions SET expires=?1 WHERE id=?2",
            params![time - 1, f.parent.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,'valid-parent-digest',?2,?3)",
            params![valid_parent.to_string(), f.account.to_string(), time + 3600],
        )
        .unwrap();
        c.execute("INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,'valid-child-digest',?2,?3,?4,?5)",params![valid_child.to_string(),f.account.to_string(),f.gateway.to_string(),valid_parent.to_string(),time+3600]).unwrap();
        for (hash, parent, expiry) in [
            ("expired-parent-code", f.parent, time + 60),
            ("expired-code", valid_parent, time - 1),
            ("valid-code", valid_parent, time + 60),
        ] {
            c.execute(
                "INSERT INTO open_codes VALUES(?1,?2,?3,?4)",
                params![hash, parent.to_string(), f.gateway.to_string(), expiry],
            )
            .unwrap();
        }
        for (hash, expiry) in [("expired-token", time - 1), ("valid-token", time + 3600)] {
            c.execute(
                "INSERT INTO tokens VALUES(?1,?2,'invite',?3)",
                params![hash, f.account.to_string(), expiry],
            )
            .unwrap();
        }
        assert_eq!(store::cleanup_expired_batch(&c, time).unwrap(), 5);
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            c.query_row("SELECT hash FROM open_codes", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "valid-code"
        );
        assert_eq!(
            c.query_row("SELECT hash FROM tokens", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "valid-token"
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM gateways", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        c.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<600) INSERT INTO tokens(hash,account,kind,expires) SELECT 'expired-batch-'||x,?1,'reset',?2 FROM n",params![f.account.to_string(),time-1]).unwrap();
    }
    assert!(f.store.active(valid_parent));
    assert!(f.store.active(valid_child));
    assert!(!f.store.active(f.parent));
    // The one-minute gate prevents repeated connections from multiplying cleanup work.
    f.store.cleanup_expired_if_due().unwrap();
    {
        let c = f.store.0.lock().unwrap();
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM tokens WHERE expires<=?1",
                [time],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            600
        );
        assert_eq!(store::cleanup_expired_batch(&c, time).unwrap(), 128);
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM tokens WHERE expires<=?1",
                [time],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            472
        );
    }
    // Initialization performs a bounded sweep as well, without changing schema1.
    let reopened = Store::open(&f.config.data_dir).unwrap();
    let c = reopened.0.lock().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM tokens WHERE expires<=?1",
            [time],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        344
    );
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM tokens WHERE hash='valid-token'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}

#[test]
fn expired_parent_fanout_cannot_expand_cleanup_beyond_row_budget() {
    let f = Fixture::new(9785);
    let time = store::now();
    let parent = Uuid::new_v4();
    let c = f.store.0.lock().unwrap();
    c.execute(
        "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,'expired-fanout-parent',?2,?3)",
        params![parent.to_string(), f.account.to_string(), time - 1],
    )
    .unwrap();
    for _ in 0..400 {
        let child = Uuid::new_v4();
        c.execute("INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,?2,?3,?4,?5,?6)",params![child.to_string(),store::secret(),f.account.to_string(),f.gateway.to_string(),parent.to_string(),time+3600]).unwrap();
        c.execute(
            "INSERT INTO open_codes VALUES(?1,?2,?3,?4)",
            params![
                store::secret(),
                parent.to_string(),
                f.gateway.to_string(),
                time + 60
            ],
        )
        .unwrap();
    }
    assert_eq!(store::cleanup_expired_batch(&c, time).unwrap(), 256);
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent=?1",
            [parent.to_string()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        272
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM open_codes WHERE parent=?1",
            [parent.to_string()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        272
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM sessions WHERE id=?1",
            [parent.to_string()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    for _ in 0..3 {
        assert!(store::cleanup_expired_batch(&c, time).unwrap() <= 512);
    }
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent=?1 OR id=?1",
            [parent.to_string()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM open_codes WHERE parent=?1",
            [parent.to_string()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn single_origin_accepts_ip_or_domain_and_rejects_insecure_public_origins() {
    let mut f = Fixture::new(9443);
    f.config.allow_insecure_loopback = false;
    for origin in [
        "https://192.0.2.10:9443",
        "https://relay.example.test",
        "https://[::1]:9443",
    ] {
        f.config.portal_origin = origin.into();
        f.config.validate().unwrap();
        assert_eq!(f.config.gateway_origin(f.gateway), Some(origin.into()));
        assert_eq!(f.config.gateway_origin(Uuid::new_v4()), Some(origin.into()));
    }
    for origin in [
        "http://192.0.2.10:9443",
        "https://example.test/path",
        "https://user@example.test",
    ] {
        f.config.portal_origin = origin.into();
        assert!(f.config.validate().is_err(), "{origin}");
    }
}

fn connected_router(f: &Fixture) -> Router {
    let state = synthetic_state(f);
    let (send, _receive) = tokio::sync::mpsc::channel(128);
    let (cancel, _stop) = tokio::sync::watch::channel(false);
    state.0.tunnels.lock().unwrap().insert(
        f.gateway,
        tunnel::Connection {
            credential: f.credential.clone(),
            workbench_api_version: WORKBENCH_API_VERSION,
            epoch: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            sessions: Default::default(),
            capacity: Arc::new(Semaphore::new(64)),
            send,
            cancel,
        },
    );
    router(state)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebind_cannot_be_replaced_by_stale_authenticated_control_admission() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let f = Fixture::new(port);
    let state = synthetic_state(&f);
    let reached = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *state.0.admission_gate.lock().unwrap() = Some(tunnel::AdmissionGate {
        credential: f.credential.clone(),
        reached: reached.clone(),
        resume: resume.clone(),
    });
    let router = router(state.clone());
    let server = tokio::spawn({
        let router = router.clone();
        async move {
            axum::serve(listener, router).await.unwrap();
        }
    });
    let mut stale = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    stale
        .send(Message::Text(
            serde_json::to_string(&Hello {
                version: VERSION,
                workbench_api_version: WORKBENCH_API_VERSION,
                gateway_id: f.gateway,
                instance_id: Uuid::new_v4(),
                client_version: "stale".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), reached.notified())
        .await
        .unwrap();
    let pending:BindingPending=serde_json::from_value(json(router.clone().oneshot(f.request(
        "/relay/v1/bindings","POST",serde_json::json!({"name":"Recovered","resumeGatewayId":f.gateway,"resumeCredential":f.credential}),false
    )).await.unwrap()).await).unwrap();
    assert_eq!(
        router
            .clone()
            .oneshot(f.request(
                "/relay/v1/bindings/approve",
                "POST",
                serde_json::json!({"userCode":pending.user_code}),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let bound: Bound = serde_json::from_value(
        json(
            router
                .clone()
                .oneshot(f.request(
                    &format!("/relay/v1/bindings/{}/poll", pending.binding_id),
                    "POST",
                    serde_json::json!({"deviceCode":pending.device_code}),
                    false,
                ))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    assert_eq!(bound.gateway_id, f.gateway);
    let mut current = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&bound.credential),
        None,
        None,
    )
    .await;
    let welcome = hello(&mut current, &f).await;
    resume.notify_one();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), stale.next()).await,
        Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_))))
    ));
    let tunnels = state.0.tunnels.lock().unwrap();
    assert_eq!(
        tunnels.get(&f.gateway).unwrap().epoch,
        welcome.connection_epoch
    );
    assert!(!*tunnels.get(&f.gateway).unwrap().cancel.borrow());
    assert!(f.store.gateway_auth(&f.credential).is_none());
    drop(tunnels);
    server.abort();
}
