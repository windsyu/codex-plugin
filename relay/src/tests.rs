use super::*;
use axum::body::Body;
use axum::http::{Request, header};
use codex_relay_protocol::{
    BindingPending, BindingPoll, BindingStart, Bound, Data, Hello, ToGateway, VERSION,
    WORKBENCH_API_VERSION, Welcome,
};
use futures_util::{SinkExt, StreamExt};
use rusqlite::params;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tower::ServiceExt;

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
            workbench_domain: "localhost".into(),
            workbench_origin: None,
            fixed_gateway_id: None,
            single_origin: false,
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
    let mut f = Fixture::new(port);
    f.config.single_origin = true;
    f.config.workbench_domain.clear();
    f
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
fn grant_request(f: &Fixture, grant: Uuid, tail: &str) -> Request<Body> {
    let mut request = f.request(
        &format!("/g/{}/workbench/v1/{tail}", f.gateway),
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
    let router = app(f.config.clone()).unwrap();
    let grant = mint_grant(&f, &router).await;
    assert_eq!(
        router
            .clone()
            .oneshot(grant_request(&f, grant, "application"))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
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
    *request.uri_mut() = format!("/g/{}/workbench/v1/application", Uuid::new_v4())
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
    let router = app(f.config.clone()).unwrap();
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

#[tokio::test]
async fn streaming_selectors_require_parent_cookie_and_strip_identity_only_after_validation() {
    let f = single_fixture(9805);
    let router = app(f.config.clone()).unwrap();
    let grant = mint_grant(&f, &router).await;
    for route in ["live/events", "terminal"] {
        let path = format!(
            "/g/{}/workbench/v1/runs/{}/{route}?relayGrant={grant}&relayApi={WORKBENCH_API_VERSION}&epoch={}",
            f.gateway,
            Uuid::new_v4(),
            Uuid::new_v4()
        );
        let request = f.request(&path, "GET", serde_json::json!({}), false);
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let mut request = f.request(&path, "GET", serde_json::json!({}), false);
        request.headers_mut().remove("cookie");
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let request = f.request(
            &format!("{path}&relayApi={WORKBENCH_API_VERSION}"),
            "GET",
            serde_json::json!({}),
            false,
        );
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
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
async fn ownership_origin_cookie_and_single_use_exchange() {
    let f = Fixture::new(9876);
    let app = app(f.config.clone()).unwrap();
    let response = app
        .clone()
        .oneshot(f.request("/relay/v1/gateways", "GET", serde_json::json!(null), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json(response).await["gateways"][0]["gatewayId"],
        f.gateway.to_string()
    );
    let path = format!("/relay/v1/gateways/{}/open", f.gateway);
    let mut req = f.request(&path, "POST", serde_json::json!({}), false);
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
    let mut req = f.request(&path, "POST", serde_json::json!({}), false);
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
    let opened = json(
        app.clone()
            .oneshot(f.request(&path, "POST", serde_json::json!({}), false))
            .await
            .unwrap(),
    )
    .await;
    let code = opened["url"]
        .as_str()
        .unwrap()
        .split("#code=")
        .nth(1)
        .unwrap();
    let request = || {
        f.request(
            "/relay/v1/session/exchange",
            "POST",
            serde_json::json!({"code":code}),
            true,
        )
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    for expected in [
        "__Host-workbench_session=",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Path=/",
    ] {
        assert!(cookie.contains(expected));
    }
    assert!(!cookie.contains("Domain="));
    assert_eq!(
        app.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut req = f.request(
        "/workbench/v1/application",
        "GET",
        serde_json::json!(null),
        true,
    );
    req.headers_mut()
        .insert("authorization", "Bearer forged".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let response = app
        .clone()
        .oneshot(f.request("/relay/v1/logout", "POST", serde_json::json!({}), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(f.store.session(&f.browser, Some(f.gateway)).is_none());
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
#[tokio::test]
async fn independent_http_sse_terminal_streams_epoch_and_logout() {
    active_streams_revocation(false).await;
}

#[tokio::test]
async fn account_deletion_closes_active_sse_and_terminal_streams() {
    active_streams_revocation(true).await;
}

async fn active_streams_revocation(delete_account: bool) {
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
                workbench_api_version: codex_relay_protocol::WORKBENCH_API_VERSION,
                gateway_id: f.gateway,
                instance_id: Uuid::new_v4(),
                client_version: "test".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let welcome: Welcome =
        serde_json::from_str(control.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://127.0.0.1:{port}/workbench/v1/application");
    let host = f.host(true);
    let cookie = format!("__Host-workbench_session={}", f.browser);
    let mut request = tokio::spawn({
        let client = client.clone();
        let host = host.clone();
        let cookie = cookie.clone();
        async move {
            client
                .get(url)
                .header("host", host)
                .header("cookie", cookie)
                .send()
                .await
                .unwrap()
        }
    });
    let open = loop {
        let m = tokio::select! {
            message = control.next() => message.unwrap().unwrap(),
            response = &mut request => panic!("HTTP request ended before stream admission: {:?}",response.unwrap().status()),
        };
        if let ToGateway::Open { request } = serde_json::from_str(m.to_text().unwrap()).unwrap() {
            break request;
        }
    };
    let mut data = socket(
        port,
        &f.host(false),
        &format!(
            "/relay/v1/tunnel/streams/{}?epoch={}",
            open.stream_id, welcome.connection_epoch
        ),
        Some(&f.credential),
        None,
        None,
    )
    .await;
    assert!(matches!(
        serde_json::from_str::<Data>(data.next().await.unwrap().unwrap().to_text().unwrap())
            .unwrap(),
        Data::RequestEnd
    ));
    data.send(Message::Text(
        serde_json::to_string(&Data::ResponseHead {
            status: 200,
            headers: vec![
                ("content-type".into(), "text/event-stream".into()),
                ("set-cookie".into(), "owner=forged".into()),
            ],
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    data.send(Message::Binary("data: first\n\n".into()))
        .await
        .unwrap();
    let mut sse_response = request.await.unwrap();
    assert_eq!(sse_response.status(), 200);
    assert!(sse_response.headers().get("set-cookie").is_none());
    assert_eq!(
        sse_response.chunk().await.unwrap().unwrap(),
        "data: first\n\n"
    );
    // An independently attached terminal works while the SSE response remains open.
    let terminal_path = format!("/workbench/v1/runs/{}/terminal", Uuid::new_v4());
    let terminal = tokio::spawn({
        let host = host.clone();
        let cookie = cookie.clone();
        let origin = f.config.gateway_origin(f.gateway).unwrap();
        async move {
            socket(
                port,
                &host,
                &terminal_path,
                None,
                Some(&cookie),
                Some(&origin),
            )
            .await
        }
    });
    let open = loop {
        let m = control.next().await.unwrap().unwrap();
        if let ToGateway::Open { request } = serde_json::from_str(m.to_text().unwrap()).unwrap() {
            break request;
        }
    };
    let mut terminal_data = socket(
        port,
        &f.host(false),
        &format!(
            "/relay/v1/tunnel/streams/{}?epoch={}",
            open.stream_id, welcome.connection_epoch
        ),
        Some(&f.credential),
        None,
        None,
    )
    .await;
    terminal_data
        .send(Message::Text(
            serde_json::to_string(&Data::WebSocketReady).unwrap().into(),
        ))
        .await
        .unwrap();
    let mut public = terminal.await.unwrap();
    public
        .send(Message::Text("synthetic input".into()))
        .await
        .unwrap();
    assert_eq!(
        terminal_data
            .next()
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
        "synthetic input"
    );
    terminal_data
        .send(Message::Binary(vec![0, 1, 2].into()))
        .await
        .unwrap();
    assert_eq!(
        public.next().await.unwrap().unwrap().into_data().as_ref(),
        &[0, 1, 2]
    );
    let (operation, token, expected) = if delete_account {
        let token = store::secret();
        let c = f.store.0.lock().unwrap();
        c.execute(
            "UPDATE accounts SET role='admin' WHERE id=?1",
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
            client.delete(format!(
                "http://127.0.0.1:{port}/relay/v1/admin/users/{}",
                f.account
            )),
            token,
            204,
        )
    } else {
        (
            client.post(format!("http://127.0.0.1:{port}/relay/v1/logout")),
            f.portal.clone(),
            200,
        )
    };
    let response = operation
        .header("host", f.host(false))
        .header("origin", &f.config.portal_origin)
        .header("cookie", format!("__Host-relay_session={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), expected);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), public.next()).await,
        Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_))))
    ));
    assert!(!f.store.active(open.session_id));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), sse_response.chunk()).await,
        Ok(Ok(None)) | Ok(Err(_))
    ));
    if delete_account {
        assert!(f.store.gateway_auth(&f.credential).is_none());
        server.abort();
        return;
    }
    // A new control connection replaces the epoch and old stream attachments fail closed.
    let mut replacement = socket(
        port,
        &f.host(false),
        "/relay/v1/tunnel",
        Some(&f.credential),
        None,
        None,
    )
    .await;
    replacement
        .send(Message::Text(
            serde_json::to_string(&Hello {
                version: VERSION,
                workbench_api_version: codex_relay_protocol::WORKBENCH_API_VERSION,
                gateway_id: f.gateway,
                instance_id: Uuid::new_v4(),
                client_version: "test".into(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let next: Welcome = serde_json::from_str(
        replacement
            .next()
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert_ne!(welcome.connection_epoch, next.connection_epoch);
    let mut forged = format!(
        "ws://127.0.0.1:{port}/relay/v1/tunnel/streams/{}?epoch={}",
        open.stream_id, welcome.connection_epoch
    )
    .into_client_request()
    .unwrap();
    forged
        .headers_mut()
        .insert("host", f.host(false).parse().unwrap());
    forged.headers_mut().insert(
        "authorization",
        format!("Bearer {}", f.credential).parse().unwrap(),
    );
    assert!(connect_async(forged).await.is_err());
    server.abort();
}

fn synthetic_state(f: &Fixture) -> App {
    App(Arc::new(Inner {
        config: f.config.clone(),
        store: Store::open(&f.config.data_dir).unwrap(),
        passwords: Arc::new(Semaphore::new(4)),
        portal_capacity: Arc::new(Semaphore::new(32)),
        control_capacity: Arc::new(Semaphore::new(256)),
        gateway_control_capacity: Mutex::new(HashMap::new()),
        stream_capacity: Arc::new(Semaphore::new(256)),
        terminal_capacity: Arc::new(Semaphore::new(16)),
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
async fn slow_request_bodies_are_admitted_before_read_and_release_capacity() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = Fixture::new(9781);
    let s = synthetic_state(&f);
    let (send, _recv) = tokio::sync::mpsc::channel(128);
    let (cancel, _stop) = tokio::sync::watch::channel(false);
    let capacity = Arc::new(Semaphore::new(64));
    s.0.tunnels.lock().unwrap().insert(
        f.gateway,
        tunnel::Connection {
            credential: f.credential.clone(),
            workbench_api_version: codex_relay_protocol::WORKBENCH_API_VERSION,
            epoch: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            sessions: Default::default(),
            capacity: capacity.clone(),
            terminal_capacity: Arc::new(Semaphore::new(4)),
            send,
            cancel,
        },
    );
    let started = Arc::new(AtomicUsize::new(0));
    let mut requests = Vec::new();
    for _ in 0..64 {
        let mut req = f.request("/workbench/v1/runs", "POST", serde_json::json!({}), true);
        let count = started.clone();
        let mut first = true;
        *req.body_mut() = Body::from_stream(futures_util::stream::poll_fn(move |_| {
            if first {
                first = false;
                count.fetch_add(1, Ordering::SeqCst);
            }
            std::task::Poll::<Option<std::result::Result<axum::body::Bytes,std::io::Error>>>::Pending
        }));
        let state = s.clone();
        let h = req.headers().clone();
        requests.push(tokio::spawn(async move {
            tunnel::forward_inner(state, h, req, None).await
        }));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while started.load(Ordering::SeqCst) < 64 {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let req = f.request("/workbench/v1/runs", "POST", serde_json::json!({}), true);
    assert!(matches!(
        tunnel::forward_inner(s.clone(), req.headers().clone(), req, None).await,
        Err(Failure(StatusCode::TOO_MANY_REQUESTS))
    ));
    assert_eq!(capacity.available_permits(), 0);
    assert_eq!(s.0.stream_capacity.available_permits(), 192);
    for task in requests {
        task.abort();
        let _ = task.await;
    }
    assert_eq!(capacity.available_permits(), 64);
    assert_eq!(s.0.stream_capacity.available_permits(), 256);
    assert!(s.0.streams.lock().unwrap().is_empty());
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
            terminal_capacity: Arc::new(Semaphore::new(4)),
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

impl Fixture {
    fn ip_trial() -> Self {
        let mut f = Self::new(9443);
        f.config.portal_origin = "https://192.0.2.10:9443".into();
        f.config.workbench_domain.clear();
        f.config.workbench_origin = Some("https://192.0.2.10:9444".into());
        f.config.fixed_gateway_id = Some(f.gateway);
        f.config.password_workers = 1;
        f.config.allow_insecure_loopback = false;
        f
    }
}

#[test]
fn fixed_ip_configuration_is_explicit_and_fails_closed() {
    let f = Fixture::ip_trial();
    f.config.validate().unwrap();
    assert_eq!(
        f.config.gateway_origin(f.gateway),
        f.config.workbench_origin
    );
    assert_eq!(f.config.gateway_origin(Uuid::new_v4()), None);
    for (portal, workbench) in [
        ("http://192.0.2.10:9443", "http://192.0.2.10:9444"),
        ("https://192.0.2.10:9443", "https://192.0.2.11:9444"),
        ("https://192.0.2.10:9443", "https://192.0.2.10:9443"),
        ("https://192.0.2.10:9443", "https://example.com:9444"),
        ("https://192.0.2.10:9443", "https://192.0.2.10:9444/path"),
    ] {
        let mut c = f.config.clone();
        c.portal_origin = portal.into();
        c.workbench_origin = Some(workbench.into());
        c.allow_insecure_loopback = true;
        assert!(c.validate().is_err(), "accepted {portal} / {workbench}");
    }
    let mut c = f.config.clone();
    c.fixed_gateway_id = None;
    assert!(c.validate().is_err());
    c.fixed_gateway_id = Some(Uuid::nil());
    assert!(c.validate().is_err());
    let mut c = f.config.clone();
    c.workbench_domain = "example.com".into();
    assert!(c.validate().is_err());
    for workers in [0, 5, usize::MAX] {
        let mut c = f.config.clone();
        c.password_workers = workers;
        assert!(c.validate().is_err());
    }
    let mut c = f.config.clone();
    c.fixed_gateway_id = Some(Uuid::new_v4());
    assert!(app(c).is_err());
    drop(app(f.config.clone()).unwrap());
    drop(app(f.config.clone()).unwrap()); // Restart preserves the configured mapping.
}

#[tokio::test]
async fn fixed_ip_ports_origins_routes_and_cookie_types_remain_separate() {
    let f = Fixture::ip_trial();
    let app = app(f.config.clone()).unwrap();
    for host in ["192.0.2.10", "192.0.2.10:9445", "192.0.2.11:9443"] {
        let mut req = f.request("/relay/v1/account", "GET", serde_json::json!({}), false);
        req.headers_mut().insert("host", host.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::MISDIRECTED_REQUEST
        );
    }
    for gateway in [false, true] {
        let mut req = f.request(
            if gateway {
                "/relay/v1/session/exchange"
            } else {
                "/relay/v1/logout"
            },
            "POST",
            serde_json::json!({"code":"invalid"}),
            gateway,
        );
        req.headers_mut().insert(
            "origin",
            if gateway {
                f.config.portal_origin.clone()
            } else {
                f.config.workbench_origin.clone().unwrap()
            }
            .parse()
            .unwrap(),
        );
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let mut req = f.request("/relay/v1/account", "GET", serde_json::json!({}), false);
    req.headers_mut().insert(
        "cookie",
        format!("__Host-relay_session={}", f.browser)
            .parse()
            .unwrap(),
    );
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut req = f.request(
        "/workbench/v1/application",
        "GET",
        serde_json::json!({}),
        true,
    );
    req.headers_mut().insert(
        "cookie",
        format!("__Host-workbench_session={}", f.portal)
            .parse()
            .unwrap(),
    );
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(f.request("/relay/v1/account", "GET", serde_json::json!({}), true))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn fixed_ip_binding_capacity_and_rebind_revoke_old_identity() {
    let f = Fixture::ip_trial();
    let app = app(f.config.clone()).unwrap();
    let pending: BindingPending = serde_json::from_value(
        json(
            app.clone()
                .oneshot(f.request(
                    "/relay/v1/bindings",
                    "POST",
                    serde_json::json!({"name":"Replacement"}),
                    false,
                ))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let approve = || {
        f.request(
            "/relay/v1/bindings/approve",
            "POST",
            serde_json::json!({"userCode":pending.user_code}),
            false,
        )
    };
    assert_eq!(
        app.clone().oneshot(approve()).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.clone()
            .oneshot(f.request(
                &format!("/relay/v1/gateways/{}", f.gateway),
                "DELETE",
                serde_json::json!({}),
                false
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert!(f.store.gateway_auth(&f.credential).is_none());
    assert!(f.store.session(&f.browser, Some(f.gateway)).is_none());
    assert_eq!(
        app.clone().oneshot(approve()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone().oneshot(approve()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let bound: Bound = serde_json::from_value(
        json(
            app.oneshot(f.request(
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
    assert_eq!(Some(bound.gateway_origin), f.config.workbench_origin);
    assert_eq!(
        f.store.gateway_auth(&bound.credential),
        Some((f.gateway, f.account))
    );
    assert!(f.store.gateway_auth(&f.credential).is_none());
    assert!(f.store.session(&f.browser, Some(f.gateway)).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fixed_uuid_rebind_cannot_be_replaced_by_stale_control_admission() {
    let f = Fixture::ip_trial();
    let s = synthetic_state(&f);
    let reached = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *s.0.admission_gate.lock().unwrap() = Some(tunnel::AdmissionGate {
        credential: f.credential.clone(),
        reached: reached.clone(),
        resume: resume.clone(),
    });
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
                client_version: "rebind-race".into(),
            })
            .unwrap()
            .into(),
        )
    };
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
    // The old credential was authenticated on a real worker. Pause it at the
    // former validation/install gap while another worker completes a rebind.
    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .unwrap();
    let old_headers = f
        .request("/", "GET", serde_json::json!({}), false)
        .headers()
        .clone();
    assert_eq!(
        portal::forget(
            State(s.clone()),
            axum::extract::Path(f.gateway),
            old_headers
        )
        .await
        .ok(),
        Some(StatusCode::NO_CONTENT)
    );
    let other_token = store::secret();
    s.0.store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                Uuid::new_v4().to_string(),
                store::digest(&other_token),
                f.other.to_string(),
                store::now() + 3600
            ],
        )
        .unwrap();
    let mut other_headers = HeaderMap::new();
    other_headers.insert(
        "cookie",
        format!("__Host-relay_session={other_token}")
            .parse()
            .unwrap(),
    );
    let axum::Json(pending) = portal::binding_start(
        State(s.clone()),
        axum::Json(BindingStart {
            name: "Other owner".into(),
            resume_gateway_id: None,
            resume_credential: None,
        }),
    )
    .await
    .ok()
    .unwrap();
    assert!(
        portal::binding_approve(
            State(s.clone()),
            other_headers,
            axum::Json(
                serde_json::from_value(serde_json::json!({"userCode":pending.user_code})).unwrap()
            )
        )
        .await
        .is_ok()
    );
    let response = portal::binding_poll(
        State(s.clone()),
        axum::extract::Path(pending.binding_id),
        axum::Json(BindingPoll {
            device_code: pending.device_code,
        }),
    )
    .await
    .ok()
    .unwrap();
    let bound: Bound = serde_json::from_value(json(response).await).unwrap();
    assert_eq!(bound.account_id, f.other);
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
    current.send(hello()).await.unwrap();
    let welcome = tokio::time::timeout(Duration::from_secs(5), current.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(welcome) = welcome else {
        panic!("missing current welcome")
    };
    let welcome: Welcome = serde_json::from_str(&welcome).unwrap();
    assert_eq!(welcome.account_id, f.other);
    resume.notify_one();
    let stale_message = tokio::time::timeout(Duration::from_secs(5), stale.next())
        .await
        .unwrap();
    assert!(
        !matches!(
            stale_message,
            Some(Ok(Message::Text(_) | Message::Binary(_)))
        ),
        "stale admission received a Welcome or grant"
    );
    let tunnels = s.0.tunnels.lock().unwrap();
    let active = tunnels.get(&f.gateway).unwrap();
    assert_eq!(active.epoch, welcome.connection_epoch);
    assert!(!*active.cancel.borrow());
    assert!(s.0.store.gateway_auth(&f.credential).is_none());
    drop(tunnels);
    server.abort();
}
