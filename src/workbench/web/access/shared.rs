//! Application-owned transport; capabilities remain in each Run's Access.
use super::*;
use std::{collections::HashMap, sync::Weak};
use tower::ServiceExt;

struct Listener {
    port: u16,
    id: Uuid,
    stop: watch::Sender<bool>,
    runs: HashMap<Uuid, (u64, Weak<WebState>)>,
    found: discovery::Found,
}
pub(crate) struct SharedAccess {
    listener: tokio::sync::Mutex<Option<Listener>>,
    setup: tokio::sync::Mutex<()>,
}
impl SharedAccess {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            listener: tokio::sync::Mutex::new(None),
            setup: tokio::sync::Mutex::new(()),
        })
    }
    pub(super) async fn serve_run(
        self: &Arc<Self>,
        state: Arc<WebState>,
        generation: u64,
        mut stop: watch::Receiver<bool>,
    ) {
        let mut global_stop = state.stop.clone();
        let result = tokio::select! {
            biased;
            _ = stop.wait_for(|v| *v) => Ok(()),
            _ = global_stop.wait_for(|v| *v) => Ok(()),
            result = self.register(&state, generation) => result,
        };
        if result.is_ok() && !*stop.borrow() && !*global_stop.borrow() {
            tokio::select! {
                _ = stop.wait_for(|v| *v) => {},
                _ = global_stop.wait_for(|v| *v) => {},
            }
        }
        self.remove(state.hub.epoch(), generation).await;
        state.access.finish(generation, result.err());
    }
    async fn register(
        self: &Arc<Self>,
        state: &Arc<WebState>,
        generation: u64,
    ) -> Result<(), &'static str> {
        // Serialize enable operations separately from request dispatch. Slow
        // configuration/discovery/bind must never hold the dispatch mutex.
        let _setup = self.setup.lock().await;
        {
            let mut guard = self.listener.lock().await;
            if let Some(listener) = guard.as_mut() {
                Self::attach(listener, state, generation);
                return Ok(());
            }
        }
        let found = state.access.discover().await;
        {
            let port = if let Some(settings) = &state.options.settings {
                let info = settings.read().await.map_err(|_| "config_unavailable")?;
                if !info.errors.is_empty() {
                    return Err("config_unavailable");
                }
                info.saved
                    .and_then(|c| c.access)
                    .map(|a| a.port)
                    .unwrap_or(0)
            } else {
                0
            };
            #[cfg(not(test))]
            let bind = Ipv4Addr::UNSPECIFIED;
            #[cfg(test)]
            let bind = Ipv4Addr::LOCALHOST;
            let listener = TcpListener::bind((bind, port))
                .await
                .map_err(|_| "listen_failed")?;
            let port = listener.local_addr().map_err(|_| "listen_failed")?.port();
            let (tx, mut rx) = watch::channel(false);
            let id = Uuid::new_v4();
            let weak = Arc::downgrade(self);
            let app = Router::new()
                .fallback(dispatch)
                .with_state((weak.clone(), id));
            let refresh = weak.clone();
            // Publish before an early server-exit callback can inspect the map.
            let mut guard = self.listener.lock().await;
            tokio::spawn(async move {
                refresh_addresses(refresh, id).await;
            });
            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = rx.wait_for(|v| *v) => {},
                    _ = axum::serve(listener, app).into_future() => {},
                }
                // Unexpected server exit also revokes every affected Run.
                if let Some(shared) = weak.upgrade() {
                    shared.end_listener(id).await;
                }
            });
            *guard = Some(Listener {
                port,
                id,
                stop: tx,
                runs: HashMap::new(),
                found,
            });
            Self::attach(guard.as_mut().unwrap(), state, generation);
        }
        Ok(())
    }
    fn attach(listener: &mut Listener, state: &Arc<WebState>, generation: u64) {
        let mut d = state.access.data.lock().unwrap();
        if d.generation != generation || d.phase != "starting" {
            return;
        }
        d.addresses = listener.found.addresses(listener.port);
        d.notice = listener.found.notice;
        d.port = Some(listener.port);
        d.phase = "ready";
        d.revision += 1;
        listener
            .runs
            .insert(state.hub.epoch(), (generation, Arc::downgrade(state)));
    }
    async fn remove(&self, id: Uuid, generation: u64) {
        let mut guard = self.listener.lock().await;
        if let Some(listener) = guard.as_mut() {
            if listener.runs.get(&id).is_some_and(|r| r.0 == generation) {
                listener.runs.remove(&id);
            }
            if listener.runs.is_empty() {
                listener.stop.send_replace(true);
                *guard = None;
            }
        }
    }
    async fn end_listener(&self, id: Uuid) {
        let mut guard = self.listener.lock().await;
        if guard.as_ref().is_some_and(|l| l.id == id)
            && let Some(listener) = guard.take()
        {
            for (generation, state) in listener.runs.into_values() {
                if let Some(state) = state.upgrade() {
                    state.access.finish(generation, Some("listener_ended"));
                }
            }
        }
    }
}
impl Drop for SharedAccess {
    fn drop(&mut self) {
        if let Some(listener) = self.listener.get_mut().take() {
            listener.stop.send_replace(true);
            for (_, state) in listener.runs.into_values() {
                if let Some(state) = state.upgrade() {
                    state.access.close();
                }
            }
        }
    }
}
async fn dispatch(
    State((shared, listener_id)): State<(Weak<SharedAccess>, Uuid)>,
    mut request: axum::extract::Request,
) -> Response {
    let Some(shared) = shared.upgrade() else {
        return error(StatusCode::NOT_FOUND, "run_unavailable");
    };
    let path = request.uri().path();
    let target = path
        .strip_prefix("/workbench/v1/runs/")
        .and_then(|r| r.split_once('/'))
        .and_then(|(id, tail)| Uuid::parse_str(id).ok().map(|id| (id, tail.to_owned())));
    let static_asset = path == "/" || path.starts_with("/assets/");
    let selected = {
        let guard = shared.listener.lock().await;
        guard
            .as_ref()
            .filter(|l| l.id == listener_id)
            .and_then(|listener| {
                if let Some((id, _)) = &target {
                    listener
                        .runs
                        .get(id)
                        .and_then(|(g, w)| w.upgrade().map(|s| (*g, s)))
                } else if static_asset {
                    listener.runs.values().find_map(|(g, w)| {
                        w.upgrade()
                            .filter(|s| {
                                s.access.valid_ingress(
                                    *g,
                                    request
                                        .headers()
                                        .get(header::HOST)
                                        .and_then(|h| h.to_str().ok())
                                        .unwrap_or(""),
                                )
                            })
                            .map(|s| (*g, s))
                    })
                } else {
                    None
                }
            })
    };
    let Some((generation, state)) = selected else {
        return error(StatusCode::NOT_FOUND, "run_unavailable");
    };
    if let Some((_, tail)) = target {
        let tail = if tail == "stop" {
            "run/stop"
        } else {
            tail.as_str()
        };
        let uri = format!(
            "/workbench/v1/{tail}{}",
            request
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        let Ok(uri) = uri.parse() else {
            return error(StatusCode::BAD_REQUEST, "invalid_run_path");
        };
        *request.uri_mut() = uri;
    }
    request.extensions_mut().insert(Ingress(generation));
    router(state).oneshot(request).await.unwrap()
}

async fn refresh_addresses(weak: Weak<SharedAccess>, id: Uuid) {
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let state = {
            let guard = shared.listener.lock().await;
            let Some(listener) = guard.as_ref().filter(|l| l.id == id) else {
                return;
            };
            listener.runs.values().find_map(|(_, w)| w.upgrade())
        };
        let Some(state) = state else {
            return;
        };
        let found = state.access.discover().await;
        let mut guard = shared.listener.lock().await;
        let Some(listener) = guard.as_mut().filter(|l| l.id == id) else {
            return;
        };
        listener.found = found.clone();
        for (generation, state) in listener.runs.values() {
            let Some(state) = state.upgrade() else {
                continue;
            };
            let mut d = state.access.data.lock().unwrap();
            if d.generation != *generation || d.phase != "ready" {
                continue;
            }
            let addresses = found.addresses(listener.port);
            if d.addresses != addresses || d.notice != found.notice {
                d.addresses = addresses;
                d.notice = found.notice;
                d.revision += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(shared: Arc<SharedAccess>) -> (Arc<WebState>, watch::Sender<bool>) {
        let (stop, rx) = watch::channel(false);
        let state = Arc::new(WebState {
            hub: LiveHub::new(crate::workbench::live::LiveLimits::default()),
            authority: "127.0.0.1:1".into(),
            origin: "http://127.0.0.1:1".into(),
            pairing: secret(),
            cookie_name: "owner".into(),
            session: secret(),
            snapshot_slots: Arc::new(Semaphore::new(2)),
            stop: rx,
            options: RuntimeOptions::default(),
            access: Access::new(),
            shared_access: Some(shared),
            terminal_slots: Arc::new(Semaphore::new(32)),
        });
        *state.access.discovery.lock().unwrap() = Some(discovery::Found {
            hosts: vec![("127.0.0.1".into(), "test".into())],
            notice: None,
        });
        let mut d = state.access.data.lock().unwrap();
        d.generation = 1;
        d.phase = "starting";
        drop(d);
        (state, stop)
    }
    async fn pair(client: &reqwest::Client, base: &str, state: &WebState) -> String {
        let response = client
            .post(format!(
                "{base}/workbench/v1/runs/{}/pair",
                state.hub.epoch()
            ))
            .header(header::ORIGIN, base)
            .body(json!({"token": state.access.pairing}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .into()
    }
    impl crate::workbench::web::RunSurface {
        pub(crate) fn loopback_device_discovery_for_test(&self) {
            *self.state.access.discovery.lock().unwrap() = Some(discovery::Found {
                hosts: vec![("127.0.0.1".into(), "test".into())],
                notice: None,
            });
        }
    }

    #[tokio::test]
    async fn shared_listener_isolates_capabilities_and_keeps_other_run_on_revocation() {
        let shared = SharedAccess::new();
        let (a, _a_stop) = run(shared.clone());
        let (b, _b_stop) = run(shared.clone());
        shared.register(&a, 1).await.unwrap();
        // A second Run must use the Application's current address directory,
        // even if its own discovery would block indefinitely.
        *b.access.discovery_hold.lock().unwrap() = Some(Arc::new(Semaphore::new(0)));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), b.access.discover())
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(1), shared.register(&b, 1))
            .await
            .expect("second Run blocked on redundant discovery")
            .unwrap();

        let port = a.access.data.lock().unwrap().port.unwrap();
        assert_eq!(b.access.data.lock().unwrap().port, Some(port));
        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let ca = pair(&client, &base, &a).await;
        let cb = pair(&client, &base, &b).await;
        let bpath = format!("/workbench/v1/runs/{}", b.hub.epoch());
        for tail in [
            "run",
            "live/snapshot",
            "live/events",
            "workspace/file?path=secret",
            "history",
            "settings",
            "requests/00000000-0000-4000-8000-000000000000",
        ] {
            let tail = if tail == "live/events" {
                format!("{tail}?epoch={}&after=0", b.hub.epoch())
            } else if tail.starts_with("requests/") {
                format!("{tail}?epoch={}", b.hub.epoch())
            } else {
                tail.to_owned()
            };
            let response = client
                .get(format!("{base}{bpath}/{tail}"))
                .header(header::COOKIE, &ca)
                .send()
                .await
                .unwrap();
            assert!(
                matches!(
                    response.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND
                ),
                "cross-Run accepted: {tail}: {}",
                response.status()
            );
        }
        for tail in [
            format!("history/{}", b.hub.epoch()),
            format!("history/{}/requests/{}", b.hub.epoch(), Uuid::new_v4()),
            format!("blob/{}", Uuid::new_v4()),
        ] {
            let denied = client
                .get(format!("{base}{bpath}/{tail}"))
                .header(header::COOKIE, &ca)
                .send()
                .await
                .unwrap();
            assert!(
                matches!(
                    denied.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND
                ),
                "cross-Run content exposed: {tail}"
            );
        }
        let denied_ws = client
            .get(format!("{base}{bpath}/terminal?epoch={}", b.hub.epoch()))
            .header(header::COOKIE, &ca)
            .header(header::ORIGIN, &base)
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .unwrap();
        assert_eq!(denied_ws.status(), StatusCode::UNAUTHORIZED);
        for tail in ["stop", "history/cleanup/preview", "history/cleanup/jobs"] {
            let denied = client
                .post(format!("{base}{bpath}/{tail}"))
                .header(header::COOKIE, &ca)
                .header(header::ORIGIN, &base)
                .header(header::CONTENT_TYPE, "application/json")
                .body("{}")
                .send()
                .await
                .unwrap();
            assert_eq!(
                denied.status(),
                StatusCode::UNAUTHORIZED,
                "cross-Run write accepted: {tail}"
            );
        }
        for path in [
            "/workbench/v1/application",
            "/workbench/v1/library/entries",
            "/workbench/v1/library/sources",
            "/workbench/v1/runs",
            "/workbench/v1/run",
        ] {
            assert_eq!(
                client
                    .get(format!("{base}{path}"))
                    .header(header::COOKIE, &ca)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        for (method, path) in [
            (reqwest::Method::POST, "/workbench/v1/launch-targets"),
            (reqwest::Method::POST, "/workbench/v1/runs"),
            (
                reqwest::Method::POST,
                "/workbench/v1/application/pick-directory",
            ),
            (reqwest::Method::POST, "/workbench/v1/library/preview"),
            (reqwest::Method::POST, "/workbench/v1/library/refresh"),
            (reqwest::Method::PUT, "/workbench/v1/application/settings"),
        ] {
            let response = client
                .request(method, format!("{base}{path}"))
                .header(header::COOKIE, &ca)
                .header(header::ORIGIN, &base)
                .header(header::CONTENT_TYPE, "application/json")
                .body("{}")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let apath = format!("/workbench/v1/runs/{}", a.hub.epoch());
        let config = client
            .get(format!("{base}{apath}/settings"))
            .header(header::COOKIE, &ca)
            .send()
            .await
            .unwrap();
        assert_eq!(config.status(), StatusCode::UNAUTHORIZED);
        let before = client
            .get(format!("{base}{apath}/run"))
            .header(header::COOKIE, &ca)
            .send()
            .await
            .unwrap();
        assert_eq!(before.status(), StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&before.bytes().await.unwrap()).unwrap()["settingsAvailable"],
            false
        );
        let wrong_pair = client
            .post(format!("{base}{bpath}/pair"))
            .header(header::ORIGIN, &base)
            .body(json!({"token":a.access.pairing}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_pair.status(), StatusCode::GONE);
        let spoof = client
            .get(format!("{base}{bpath}/run"))
            .header(header::HOST, &b.authority)
            .header(header::COOKIE, format!("{}={}", b.cookie_name, b.session))
            .send()
            .await
            .unwrap();
        assert_eq!(spoof.status(), StatusCode::FORBIDDEN);
        let mut stream = client
            .get(format!(
                "{base}{apath}/live/events?epoch={}&after=0",
                a.hub.epoch()
            ))
            .header(header::COOKIE, &ca)
            .send()
            .await
            .unwrap();
        assert_eq!(stream.status(), StatusCode::OK);
        a.access.close();
        let ended = tokio::time::timeout(Duration::from_secs(1), async {
            while stream.chunk().await.unwrap().is_some() {}
        })
        .await;
        assert!(ended.is_ok(), "revoked Run SSE remained open");
        shared.remove(a.hub.epoch(), 1).await;
        assert_eq!(
            client
                .get(format!("{base}{apath}/run"))
                .header(header::COOKIE, &ca)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .get(format!("{base}{bpath}/run"))
                .header(header::COOKIE, &cb)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        // A can reopen on B's existing transport without reviving A's old
        // session. Late cleanup from A's previous generation is harmless.
        {
            let mut d = a.access.data.lock().unwrap();
            d.generation = 2;
            d.phase = "starting";
        }
        shared.register(&a, 2).await.unwrap();
        shared.remove(a.hub.epoch(), 1).await;
        assert_eq!(
            client
                .get(format!("{base}{apath}/run"))
                .header(header::COOKIE, &ca)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let ca2 = pair(&client, &base, &a).await;
        assert_eq!(
            client
                .get(format!("{base}{apath}/run"))
                .header(header::COOKIE, &ca2)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .get(format!("{base}{bpath}/run"))
                .header(header::COOKIE, &cb)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        a.access.close();
        shared.remove(a.hub.epoch(), 2).await;
        b.access.close();
        shared.remove(b.hub.epoch(), 1).await;
        assert!(shared.listener.lock().await.is_none());
    }
}
