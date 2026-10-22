use super::*;
use crate::workbench::relay::channel_tests::SecurePeer;
use crate::workbench::{relay::Snapshot, remote_access::Ingress};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{
        State, WebSocketUpgrade,
        ws::{Message as ServerMessage, WebSocket},
    },
    http::StatusCode,
    routing::get,
};
use codex_relay_protocol::secure::{self, AccessContext, ChannelBinding, Frame, Scope};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

#[derive(Clone)]
struct Endpoints {
    control: mpsc::UnboundedSender<WebSocket>,
    data: mpsc::UnboundedSender<WebSocket>,
}
struct Fixture {
    instance: Uuid,
    ingress: Ingress,
    saved: Saved,
    view: Shared,
    welcome: Welcome,
    control: mpsc::UnboundedReceiver<WebSocket>,
    data: mpsc::UnboundedReceiver<WebSocket>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(local: Router) -> Self {
        let instance = Uuid::new_v4();
        let ingress = Ingress::start(local, instance).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let welcome = Welcome {
            version: protocol::VERSION,
            account_id: Uuid::new_v4(),
            gateway_id: Uuid::new_v4(),
            connection_epoch: Uuid::new_v4(),
            gateway_origin: origin.clone(),
        };
        let saved = Saved {
            server_url: origin,
            name: "Synthetic reconnect fixture".into(),
            allow_insecure_loopback: true,
            enabled: true,
            authentication_required: false,
            bound: Some(protocol::Bound {
                username: "synthetic".into(),
                account_id: welcome.account_id,
                gateway_id: welcome.gateway_id,
                gateway_origin: welcome.gateway_origin.clone(),
                credential: "synthetic-credential".into(),
            }),
        };
        let view = Arc::new(Mutex::new(Snapshot::from_saved(&saved)));
        let (control_tx, control) = mpsc::unbounded_channel();
        let (data_tx, data) = mpsc::unbounded_channel();
        let app = Router::new()
            .route(
                "/relay/v1/tunnel",
                get(
                    |State(state): State<Endpoints>, ws: WebSocketUpgrade| async {
                        ws.on_upgrade(move |socket| async move {
                            let _ = state.control.send(socket);
                        })
                    },
                ),
            )
            .route(
                "/relay/v1/tunnel/channels/{id}",
                get(
                    |State(state): State<Endpoints>, ws: WebSocketUpgrade| async {
                        ws.on_upgrade(move |socket| async move {
                            let _ = state.data.send(socket);
                        })
                    },
                ),
            )
            .with_state(Endpoints {
                control: control_tx,
                data: data_tx,
            });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            instance,
            ingress,
            saved,
            view,
            welcome,
            control,
            data,
            server,
        }
    }
    fn spawn(&self, reconnect: bool) -> tokio::task::JoinHandle<bool> {
        let (saved, instance, ingress, view) = (
            self.saved.clone(),
            self.instance,
            self.ingress.handle(),
            self.view.clone(),
        );
        tokio::spawn(async move {
            if reconnect {
                run(saved, instance, ingress, view).await
            } else {
                connection(&saved, instance, &ingress, &view).await.is_ok()
            }
        })
    }
    async fn handshake(&mut self) -> WebSocket {
        let mut socket = timeout(Duration::from_secs(3), self.control.recv())
            .await
            .unwrap()
            .unwrap();
        let Some(Ok(ServerMessage::Text(hello))) = socket.recv().await else {
            panic!("missing hello");
        };
        let hello: Hello = serde_json::from_str(&hello).unwrap();
        assert_eq!(hello.instance_id, self.instance);
        server_json(&mut socket, &self.welcome).await;
        socket
    }
    fn request(&self) -> ChannelBinding {
        ChannelBinding {
            channel_id: Uuid::new_v4(),
            scope: Scope::Application,
            context: AccessContext {
                wire_version: protocol::VERSION,
                api_version: protocol::WORKBENCH_API_VERSION,
                account_id: self.welcome.account_id,
                parent_session_id: Uuid::new_v4(),
                grant_id: Uuid::new_v4(),
                gateway_id: self.welcome.gateway_id,
                instance_id: self.instance,
                connection_epoch: self.welcome.connection_epoch,
            },
        }
    }
    async fn channel(&mut self, control: &mut WebSocket, binding: &ChannelBinding) -> SecurePeer {
        server_json(
            control,
            &ToGateway::OpenChannel {
                binding: binding.clone(),
            },
        )
        .await;
        let data = timeout(Duration::from_secs(3), self.data.recv())
            .await
            .unwrap()
            .unwrap();
        SecurePeer::mock(data, binding).await.unwrap()
    }
}
async fn server_json(socket: &mut WebSocket, value: &impl serde::Serialize) {
    socket
        .send(ServerMessage::Text(
            serde_json::to_string(value).unwrap().into(),
        ))
        .await
        .unwrap();
}
async fn closed(socket: &mut SecurePeer) {
    assert!(
        timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("old encrypted channel must close")
            .is_err()
    );
}

#[test]
fn backoff_is_bounded_jittered_and_repeated_stable_recovery_resets_it() {
    let mut backoff = Backoff::default();
    for _ in 0..3 {
        for ceiling in [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000] {
            let delay = backoff.next_delay().as_millis();
            assert!((ceiling / 2..=ceiling).contains(&delay));
        }
        let now = Instant::now();
        backoff.connected(now);
        backoff.control_received(now + Duration::from_secs(protocol::RENEW_SECONDS - 1));
        assert_eq!(
            backoff.ceiling_ms, 30_000,
            "short-lived welcome must not reset failures"
        );
        backoff.control_received(now + Duration::from_secs(protocol::RENEW_SECONDS));
        assert_eq!(backoff.ceiling_ms, 1_000);
    }
}

#[tokio::test]
async fn identity_version_and_nil_epoch_never_become_online() {
    for mismatch in 0..5 {
        let mut fixture = Fixture::new(Router::new()).await;
        match mismatch {
            0 => fixture.welcome.version += 1,
            1 => fixture.welcome.account_id = Uuid::new_v4(),
            2 => fixture.welcome.gateway_id = Uuid::new_v4(),
            3 => fixture.welcome.gateway_origin = "https://different.example".into(),
            _ => fixture.welcome.connection_epoch = Uuid::nil(),
        }
        let task = fixture.spawn(false);
        let _control = fixture.handshake().await;
        assert!(
            !timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
        );
        assert_ne!(fixture.view.lock().unwrap().state, "online");
        assert!(fixture.ingress.handle().grant(Uuid::new_v4()).is_err());
    }
}

#[tokio::test]
async fn renewed_connections_restore_fast_recovery_after_repeated_failures() {
    let mut backoff = Backoff::default();
    for _ in 0..2 {
        for _ in 0..6 {
            backoff.next_delay();
        }
        assert_eq!(backoff.ceiling_ms, 30_000);
        let mut fixture = Fixture::new(Router::new()).await;
        let (saved, instance, ingress, view) = (
            fixture.saved.clone(),
            fixture.instance,
            fixture.ingress.handle(),
            fixture.view.clone(),
        );
        let task = tokio::spawn(async move {
            assert!(
                connection_attempt(&saved, instance, &ingress, &view, &mut backoff)
                    .await
                    .is_err()
            );
            backoff
        });
        let mut control = fixture.handshake().await;
        timeout(Duration::from_secs(2), async {
            while fixture.view.lock().unwrap().state != "online" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(
            Duration::from_secs(protocol::RENEW_SECONDS) + Duration::from_millis(20),
        )
        .await;
        server_json(
            &mut control,
            &ToGateway::Renew {
                session_ids: vec![],
            },
        )
        .await;
        control.send(ServerMessage::Close(None)).await.unwrap();
        backoff = timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(backoff.ceiling_ms, 1_000);
        assert!(backoff.next_delay() <= Duration::from_secs(1));
    }
}

#[tokio::test]
async fn blackholed_control_expires_at_lease_deadline_and_cancels_active_sse() {
    let local = Router::new().route(
        "/workbench/v1/application",
        get(|| async {
            axum::response::Response::builder()
                .header("Content-Type", "application/json")
                .body(Body::from_stream(futures_util::stream::pending::<
                    Result<Bytes, std::convert::Infallible>,
                >()))
                .unwrap()
        }),
    );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let request = fixture.request();
    let mut data = fixture.channel(&mut control, &request).await;
    data.request(1, "GET", "/workbench/v1/application", vec![], &[])
        .await
        .unwrap();
    assert!(matches!(
        data.next().await.unwrap(),
        Frame::Head {
            id: 1,
            status: 200,
            ..
        }
    ));
    // Move the lease deadline off the heartbeat tick. The old tick-only timeout
    // waits almost another whole renewal interval before fencing this connection.
    tokio::time::sleep(Duration::from_millis(150)).await;
    server_json(
        &mut control,
        &ToGateway::Renew {
            session_ids: vec![request.context.grant_id],
        },
    )
    .await;
    assert!(
        !timeout(Duration::from_secs(protocol::LEASE_SECONDS + 1), task)
            .await
            .expect("lease expiry must not wait for the next heartbeat tick")
            .unwrap()
    );
    assert!(
        fixture
            .ingress
            .handle()
            .grant(request.context.grant_id)
            .is_err()
    );
    closed(&mut data).await;
}

#[tokio::test]
async fn reconnect_discards_old_stream_and_capability_and_abort_stops_recovery() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    let local = Router::new().route(
        "/workbench/v1/application",
        get(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::Json(serde_json::json!({"accepted":true}))
            }
        }),
    );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(true);
    let mut control = fixture.handshake().await;
    let request = fixture.request();
    let mut data = fixture.channel(&mut control, &request).await;
    let old_capability = fixture
        .ingress
        .handle()
        .grant(request.context.grant_id)
        .unwrap();
    data.send(&Frame::Open {
        id: 1,
        kind: secure::Kind::Http,
        method: "GET".into(),
        path: "/workbench/v1/application".into(),
        headers: vec![],
        body_length: 32,
        receive_window: secure::WINDOW,
    })
    .await
    .unwrap();
    data.accepted(1).await.unwrap();
    data.send(&Frame::Chunk {
        id: 1,
        data: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"never-completed-input",
        ),
    })
    .await
    .unwrap();
    control.send(ServerMessage::Close(None)).await.unwrap();
    fixture.welcome.connection_epoch = Uuid::new_v4();
    let _new_control = fixture.handshake().await;
    closed(&mut data).await;
    timeout(Duration::from_secs(2), async {
        loop {
            if fixture.view.lock().unwrap().state == "online" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let fresh_capability = fixture
        .ingress
        .handle()
        .grant(request.context.grant_id)
        .unwrap();
    assert_ne!(fresh_capability, old_capability);
    let status = Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!(
            "http://{}/workbench/v1/application",
            fixture.ingress.handle().address()
        ))
        .header(
            "host",
            Url::parse(&fixture.welcome.gateway_origin)
                .unwrap()
                .authority(),
        )
        .bearer_auth(old_capability)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        timeout(Duration::from_millis(150), fixture.data.recv())
            .await
            .is_err(),
        "new epoch must not reconnect old streams"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "incomplete HTTP input must never replay"
    );
    task.abort();
    let _ = task.await;
    assert!(
        fixture
            .ingress
            .handle()
            .grant(request.context.grant_id)
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(1_100), fixture.control.recv())
            .await
            .is_err(),
        "cancelled connector must stay stopped"
    );
}

#[tokio::test]
async fn permanent_credential_rejections_do_not_reconnect() {
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        let fixture = Fixture::new(Router::new()).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut saved = fixture.saved.clone();
        saved.server_url = format!("http://{}", listener.local_addr().unwrap());
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen = attempts.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/relay/v1/tunnel",
                    get(move || {
                        let seen = seen.clone();
                        async move {
                            seen.fetch_add(1, Ordering::SeqCst);
                            status
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        assert!(
            timeout(
                Duration::from_secs(2),
                run(
                    saved,
                    fixture.instance,
                    fixture.ingress.handle(),
                    fixture.view.clone()
                )
            )
            .await
            .unwrap()
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(fixture.ingress.handle().grant(Uuid::new_v4()).is_err());
        server.abort();
    }
}

#[tokio::test]
async fn terminal_input_is_delivered_once_and_never_replayed_after_epoch_change() {
    let run_id = Uuid::new_v4();
    let path = format!("/workbench/v1/runs/{run_id}/terminal");
    let (received, mut input) = mpsc::unbounded_channel();
    let local = Router::new().route(
        &path,
        get(move |ws: WebSocketUpgrade| {
            let received = received.clone();
            async move {
                ws.on_upgrade(move |mut socket| async move {
                    while let Some(Ok(ServerMessage::Text(text))) = socket.recv().await {
                        let _ = received.send(text.to_string());
                    }
                })
            }
        }),
    );
    let mut fixture = Fixture::new(local).await;
    let life = crate::workbench::permission::Permission::new();
    fixture
        .ingress
        .handle()
        .set_run_resolver(Arc::new(move |id| (id == run_id).then(|| life.clone())));
    let task = fixture.spawn(true);
    let mut control = fixture.handshake().await;
    let mut request = fixture.request();
    request.scope = Scope::Run { run_id };
    let mut data = fixture
        .channel(&mut control, &request)
        .await
        .terminal(1, &path)
        .await
        .unwrap();
    data.send(Message::Text("synthetic terminal input".into()))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), input.recv())
            .await
            .unwrap()
            .unwrap(),
        "synthetic terminal input"
    );
    control.send(ServerMessage::Close(None)).await.unwrap();
    fixture.welcome.connection_epoch = Uuid::new_v4();
    let _new_control = fixture.handshake().await;
    assert!(
        timeout(Duration::from_secs(2), data.next())
            .await
            .expect("old encrypted terminal must close")
            .is_none_or(|message| message.is_err())
    );
    assert!(
        timeout(Duration::from_millis(150), fixture.data.recv())
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(150), input.recv())
            .await
            .is_err()
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn abort_during_handshake_fences_late_welcome_and_prevents_reconnect() {
    let mut fixture = Fixture::new(Router::new()).await;
    let task = fixture.spawn(true);
    let mut control = timeout(Duration::from_secs(2), fixture.control.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        control.recv().await,
        Some(Ok(ServerMessage::Text(_)))
    ));
    task.abort();
    let _ = task.await;
    // The server may have already queued this result when cancellation occurred.
    let _ = control
        .send(ServerMessage::Text(
            serde_json::to_string(&fixture.welcome).unwrap().into(),
        ))
        .await;
    assert_ne!(fixture.view.lock().unwrap().state, "online");
    assert!(fixture.ingress.handle().grant(Uuid::new_v4()).is_err());
    assert!(
        timeout(Duration::from_millis(1_100), fixture.control.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn idle_sse_observes_remote_close_and_rejects_second_request_end() {
    for invalid_frame in [false, true] {
        let local = Router::new()
            .route(
                "/workbench/v1/application",
                get(|| async {
                    axum::response::Response::builder()
                        .header("Content-Type", "application/json")
                        .body(Body::from_stream(futures_util::stream::pending::<
                            Result<Bytes, std::convert::Infallible>,
                        >()))
                        .unwrap()
                }),
            )
            .route(
                "/workbench/v1/application/settings",
                get(|| async { axum::Json(serde_json::json!({"alive":true})) }),
            );
        let mut fixture = Fixture::new(local).await;
        let task = fixture.spawn(false);
        let mut control = fixture.handshake().await;
        let request = fixture.request();
        let mut data = fixture.channel(&mut control, &request).await;
        data.request(1, "GET", "/workbench/v1/application", vec![], &[])
            .await
            .unwrap();
        assert!(matches!(
            data.next().await.unwrap(),
            Frame::Head {
                id: 1,
                status: 200,
                ..
            }
        ));
        if invalid_frame {
            data.send(&Frame::End { id: 1 }).await.unwrap();
            assert!(matches!(
                data.next().await.unwrap(),
                Frame::Reset { id: 1, .. }
            ));
            // Authenticated late records still advance the channel sequence but
            // cannot resurrect the closed child or break unrelated work.
            data.send(&Frame::End { id: 1 }).await.unwrap();
            data.request(2, "GET", "/workbench/v1/application/settings", vec![], &[])
                .await
                .unwrap();
            let response = data.response(2, "GET").await.unwrap();
            assert_eq!(response.status(), 200);
            let body: serde_json::Value =
                serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            assert_eq!(body["alive"], true);
        } else {
            data.close().await.unwrap();
            timeout(Duration::from_secs(2), async {
                loop {
                    match control.recv().await {
                        Some(Ok(ServerMessage::Text(text)))
                            if matches!(
                                serde_json::from_str::<ToRelay>(&text),
                                Ok(ToRelay::ChannelFailed { .. })
                            ) =>
                        {
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("idle channel close must release its desktop worker");
        }
        assert!(
            !task.is_finished(),
            "one child or data-channel failure must not kill the control connection"
        );
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn disable_and_forget_during_tunnel_handshake_cannot_be_undone_by_late_welcome() {
    use crate::workbench::relay::{Service, storage::Storage};
    for forget in [false, true] {
        let mut fixture = Fixture::new(Router::new()).await;
        let root = tempfile::tempdir().unwrap();
        Storage::open(root.path())
            .unwrap()
            .save(&fixture.saved)
            .unwrap();
        let service = Service::start(
            root.path().into(),
            fixture.instance,
            fixture.ingress.handle(),
        )
        .unwrap();
        let mut control = timeout(Duration::from_secs(2), fixture.control.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            control.recv().await,
            Some(Ok(ServerMessage::Text(_)))
        ));
        let result = if forget {
            service.forget().await
        } else {
            service.disable().await
        }
        .unwrap();
        assert!(!result.enabled);
        let _ = control
            .send(ServerMessage::Text(
                serde_json::to_string(&fixture.welcome).unwrap().into(),
            ))
            .await;
        assert!(
            timeout(Duration::from_millis(1_100), fixture.control.recv())
                .await
                .is_err()
        );
        assert_eq!(service.status().await.state, "disabled");
        assert!(fixture.ingress.handle().grant(Uuid::new_v4()).is_err());
        let persisted = Storage::open(root.path()).unwrap().load().unwrap();
        assert!(!persisted.enabled);
        assert_eq!(persisted.bound.is_none(), forget);
    }
}

#[tokio::test]
async fn suspended_or_failed_clock_fences_ready_renew_and_open_before_acceptance() {
    for failure in [
        Some(Duration::from_secs(131)),
        None,
        Some(Duration::from_secs(99)),
    ] {
        for open in [false, true] {
            let mut fixture = Fixture::new(Router::new()).await;
            let clock = Clock::manual(Duration::from_secs(100));
            let connector_clock = clock.clone();
            let (saved, instance, ingress, view) = (
                fixture.saved.clone(),
                fixture.instance,
                fixture.ingress.handle(),
                fixture.view.clone(),
            );
            let task = tokio::spawn(async move {
                connection_with_clock(
                    &saved,
                    instance,
                    &ingress,
                    &view,
                    &mut Backoff::default(),
                    connector_clock,
                )
                .await
            });
            let mut control = fixture.handshake().await;
            timeout(Duration::from_secs(2), async {
                while fixture.view.lock().unwrap().state != "online" {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let request = fixture.request();
            fixture
                .ingress
                .handle()
                .grant(request.context.grant_id)
                .unwrap();
            let awake = Instant::now();
            clock.set(failure);
            let message = if open {
                ToGateway::OpenChannel {
                    binding: request.clone(),
                }
            } else {
                ToGateway::Renew {
                    session_ids: vec![request.context.grant_id],
                }
            };
            server_json(&mut control, &message).await;
            assert!(
                timeout(Duration::from_secs(2), task)
                    .await
                    .expect("continuous oracle must fence without waiting for awake timer")
                    .unwrap()
                    .is_err()
            );
            assert!(awake.elapsed() < Duration::from_secs(2));
            assert!(
                fixture
                    .ingress
                    .handle()
                    .grant(request.context.grant_id)
                    .is_err()
            );
            assert!(
                fixture.data.try_recv().is_err(),
                "stale Open must never create a data connection"
            );
        }
    }
}

#[tokio::test]
async fn suspend_during_handshake_cannot_accept_buffered_welcome_as_fresh_authority() {
    let mut fixture = Fixture::new(Router::new()).await;
    let clock = Clock::manual(Duration::from_secs(100));
    let connector_clock = clock.clone();
    let (saved, instance, ingress, view) = (
        fixture.saved.clone(),
        fixture.instance,
        fixture.ingress.handle(),
        fixture.view.clone(),
    );
    let task = tokio::spawn(async move {
        connection_with_clock(
            &saved,
            instance,
            &ingress,
            &view,
            &mut Backoff::default(),
            connector_clock,
        )
        .await
    });
    let mut control = timeout(Duration::from_secs(2), fixture.control.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        control.recv().await,
        Some(Ok(ServerMessage::Text(_)))
    ));
    clock.set(Some(Duration::from_secs(131)));
    server_json(&mut control, &fixture.welcome).await;
    assert!(
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_ne!(fixture.view.lock().unwrap().state, "online");
    assert!(fixture.ingress.handle().grant(Uuid::new_v4()).is_err());
}

#[tokio::test]
async fn authenticated_request_end_is_required_before_any_mutation() {
    for missing_end in [true, false] {
        let executed = Arc::new(AtomicUsize::new(0));
        let count = executed.clone();
        let local = Router::new().route(
            "/workbench/v1/application/settings",
            axum::routing::put(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({"accepted":true}))
                }
            }),
        );
        let mut fixture = Fixture::new(local).await;
        let task = fixture.spawn(false);
        let mut control = fixture.handshake().await;
        let binding = fixture.request();
        let mut peer = fixture.channel(&mut control, &binding).await;
        peer.send(&Frame::Open {
            id: 1,
            kind: secure::Kind::Http,
            method: "PUT".into(),
            path: "/workbench/v1/application/settings".into(),
            headers: vec![],
            body_length: if missing_end { 2 } else { 3 },
            receive_window: secure::WINDOW,
        })
        .await
        .unwrap();
        peer.accepted(1).await.unwrap();
        peer.send(&Frame::Chunk {
            id: 1,
            data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"{}"),
        })
        .await
        .unwrap();
        if missing_end {
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(
                executed.load(Ordering::SeqCst),
                0,
                "complete bytes without authenticated End must not execute"
            );
            peer.close().await.unwrap();
            timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(Ok(ServerMessage::Text(text))) = control.recv().await
                        && matches!(serde_json::from_str::<ToRelay>(&text), Ok(ToRelay::ChannelFailed { channel_id, .. }) if channel_id == binding.channel_id)
                    {
                        break;
                    }
                }
            }).await.expect("disconnect must finish the incomplete request worker");
            assert_eq!(
                executed.load(Ordering::SeqCst),
                0,
                "EOF without End must not execute either"
            );
        } else {
            peer.send(&Frame::End { id: 1 }).await.unwrap();
            assert!(matches!(
                peer.next().await.unwrap(),
                Frame::Reset { id: 1, .. }
            ));
            assert_eq!(
                executed.load(Ordering::SeqCst),
                0,
                "short request must not execute"
            );
            peer.request(2, "PUT", "/workbench/v1/application/settings", vec![], &[])
                .await
                .unwrap();
            let response = peer.response(2, "PUT").await.unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&response.bytes().await.unwrap())
                    .unwrap()["accepted"],
                true
            );
            assert_eq!(executed.load(Ordering::SeqCst), 1);
        }
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn inner_forbidden_cancel_and_zero_credit_do_not_poison_other_children() {
    let local = Router::new()
        .route(
            "/workbench/v1/application",
            get(|| async { axum::Json(serde_json::json!({"large":"x".repeat(128*1024)})) }),
        )
        .route(
            "/workbench/v1/application/settings",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({"error":{"code":"policy_disabled"}})),
                )
            }),
        );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let binding = fixture.request();
    let mut peer = fixture.channel(&mut control, &binding).await;
    peer.send(&Frame::Open {
        id: 1,
        kind: secure::Kind::Http,
        method: "GET".into(),
        path: "/workbench/v1/application".into(),
        headers: vec![],
        body_length: 0,
        receive_window: 0,
    })
    .await
    .unwrap();
    peer.accepted(1).await.unwrap();
    peer.send(&Frame::End { id: 1 }).await.unwrap();
    assert!(matches!(
        peer.next().await.unwrap(),
        Frame::Head {
            id: 1,
            status: 200,
            ..
        }
    ));
    // Child 1 cannot consume even one byte; the channel reader/writer must still
    // authenticate and finish child 2's business-level 403 independently.
    peer.request(2, "GET", "/workbench/v1/application/settings", vec![], &[])
        .await
        .unwrap();
    assert!(matches!(
        peer.next().await.unwrap(),
        Frame::Head {
            id: 2,
            status: 403,
            ..
        }
    ));
    let mut body = Vec::new();
    loop {
        match peer.next().await.unwrap() {
            Frame::Chunk { id: 2, data } => body.extend(
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).unwrap(),
            ),
            Frame::End { id: 2 } => break,
            _ => panic!("sibling response was disturbed"),
        }
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["code"],
        "policy_disabled"
    );
    assert!(
        fixture
            .ingress
            .handle()
            .permission(binding.context.grant_id)
            .unwrap()
            .active()
    );
    peer.send(&Frame::Reset {
        id: 1,
        code: "cancelled".into(),
    })
    .await
    .unwrap();
    peer.send(&Frame::Window { id: 1, credit: 1 })
        .await
        .unwrap();
    peer.request(3, "GET", "/workbench/v1/application/settings", vec![], &[])
        .await
        .unwrap();
    let response = peer.response(3, "GET").await.unwrap();
    assert_eq!(response.status(), 403);
    assert!(
        response.bytes().await.is_ok(),
        "Cancel and late authenticated records must not break sibling framing"
    );
    assert!(!task.is_finished());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn truncated_local_response_resets_child_without_authenticated_response_end() {
    let fail = Arc::new(tokio::sync::Notify::new());
    let signal = fail.clone();
    let local = Router::new()
        .route(
            "/workbench/v1/application",
            get(move || {
                let signal = signal.clone();
                async move {
                    let stream = async_stream::stream! {
                        yield Ok::<_,std::io::Error>(Bytes::from_static(b"{\"prefix\":"));
                        signal.notified().await;
                        yield Err(std::io::Error::other("synthetic source truncation"));
                    };
                    axum::response::Response::builder()
                        .header("Content-Type", "application/json")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
            }),
        )
        .route(
            "/workbench/v1/application/settings",
            get(|| async { axum::Json(serde_json::json!({"ok":true})) }),
        );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let binding = fixture.request();
    let mut peer = fixture.channel(&mut control, &binding).await;
    peer.request(1, "GET", "/workbench/v1/application", vec![], &[])
        .await
        .unwrap();
    assert!(matches!(
        peer.next().await.unwrap(),
        Frame::Head {
            id: 1,
            status: 200,
            ..
        }
    ));
    assert!(matches!(
        peer.next().await.unwrap(),
        Frame::Chunk { id: 1, .. }
    ));
    fail.notify_one();
    assert!(
        matches!(peer.next().await.unwrap(), Frame::Reset { id: 1, .. }),
        "a truncated source must not become ResponseEnd"
    );
    peer.request(2, "GET", "/workbench/v1/application/settings", vec![], &[])
        .await
        .unwrap();
    assert_eq!(
        peer.response(2, "GET")
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        "{\"ok\":true}"
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn bodyless_responses_still_have_an_authenticated_response_end() {
    let local = Router::new()
        .route(
            "/workbench/v1/application",
            get(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/workbench/v1/application/settings",
            get(|| async { StatusCode::NOT_MODIFIED }),
        );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let binding = fixture.request();
    let mut peer = fixture.channel(&mut control, &binding).await;
    for (id, path, status) in [
        (1, "/workbench/v1/application", 204),
        (2, "/workbench/v1/application/settings", 304),
    ] {
        peer.request(id, "GET", path, vec![], &[]).await.unwrap();
        assert!(
            matches!(peer.next().await.unwrap(),Frame::Head{id:actual,status:actual_status,..} if actual==id&&actual_status==status)
        );
        assert!(
            matches!(peer.next().await.unwrap(),Frame::End{id:actual} if actual==id),
            "status-only responses must not omit authenticated completion"
        );
    }
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn finite_response_larger_than_request_cap_streams_and_releases_reservations() {
    const DATA_CHUNKS: usize = 193;
    let local = Router::new()
        .route(
            "/workbench/v1/application",
            get(|| async {
                let chunk = Bytes::from(vec![b'x'; secure::CHUNK]);
                let stream = futures_util::stream::iter(0..DATA_CHUNKS + 2).map(move |index| {
                    Ok::<_, std::convert::Infallible>(if index == 0 || index == DATA_CHUNKS + 1 {
                        Bytes::from_static(b"\"")
                    } else {
                        chunk.clone()
                    })
                });
                axum::response::Response::builder()
                    .header("Content-Type", "application/json")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        )
        .route(
            "/workbench/v1/application/settings",
            // Consume the whole local request before replying. Early responses
            // with an unread 2 MiB HTTP body may close the connection while the
            // client is still writing, which tests Hyper rather than our budget.
            axum::routing::put(|body: Bytes| async move {
                axum::Json(serde_json::json!({"accepted":true,"length":body.len()}))
            }),
        );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let binding = fixture.request();
    let mut peer = fixture.channel(&mut control, &binding).await;
    peer.request(1, "GET", "/workbench/v1/application", vec![], &[])
        .await
        .unwrap();
    assert!(matches!(
        peer.next().await.unwrap(),
        Frame::Head {
            id: 1,
            status: 200,
            ..
        }
    ));
    let mut received = 0;
    loop {
        match peer.next().await.unwrap() {
            Frame::Chunk { id: 1, data } => {
                let bytes =
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                        .unwrap();
                received += bytes.len();
                peer.send(&Frame::Window {
                    id: 1,
                    credit: bytes.len(),
                })
                .await
                .unwrap();
            }
            Frame::End { id: 1 } => break,
            _ => panic!("large finite response must remain a normal authenticated stream"),
        }
    }
    assert_eq!(received, DATA_CHUNKS * secure::CHUNK + 2);
    assert!(
        received > protocol::MAX_HTTP_BODY,
        "request body cap must not become a finite response cap"
    );
    // More than 32MiB cumulatively crosses the same live channel. A leaked
    // request/reassembly reservation would reject a later Open despite no
    // concurrent children. Every response is consumed through authenticated End.
    let request = vec![b' '; protocol::MAX_HTTP_BODY];
    for id in 2..=20 {
        peer.request(
            id,
            "PUT",
            "/workbench/v1/application/settings",
            vec![],
            &request,
        )
        .await
        .unwrap();
        let mut head = false;
        let mut body = Vec::new();
        loop {
            match peer.next().await.unwrap() {
                Frame::Window { id: actual, .. } if actual == id => {}
                Frame::Head {
                    id: actual,
                    status: 200,
                    ..
                } if actual == id => {
                    assert!(!head);
                    head = true;
                }
                Frame::Chunk { id: actual, data } if actual == id => {
                    assert!(head);
                    let bytes =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                            .unwrap();
                    peer.send(&Frame::Window {
                        id,
                        credit: bytes.len(),
                    })
                    .await
                    .unwrap();
                    body.extend(bytes);
                }
                Frame::End { id: actual } if actual == id => {
                    assert!(head);
                    break;
                }
                frame => panic!(
                    "completed transport allocations must be reusable: id={id}, frame={}",
                    serde_json::to_string(&frame).unwrap()
                ),
            }
        }
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["accepted"], true);
        assert_eq!(response["length"], protocol::MAX_HTTP_BODY);
        if id % 4 == 0 {
            server_json(
                &mut control,
                &ToGateway::Renew {
                    session_ids: vec![binding.context.grant_id],
                },
            )
            .await;
        }
    }
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn run_retirement_and_exact_scope_do_not_revoke_application_or_other_run() {
    use crate::workbench::permission::Permission;
    let run_a = Uuid::new_v4();
    let run_b = Uuid::new_v4();
    let life_a = Permission::new();
    let life_b = Permission::new();
    let path_a = format!("/workbench/v1/runs/{run_a}/run");
    let path_b = format!("/workbench/v1/runs/{run_b}/run");
    let local = Router::new()
        .route(
            "/workbench/v1/application",
            get(|| async { axum::Json(serde_json::json!({"ok":true})) }),
        )
        .route(
            &path_a,
            get(|| async { axum::Json(serde_json::json!({"ok":true})) }),
        )
        .route(
            &path_b,
            get(|| async { axum::Json(serde_json::json!({"ok":true})) }),
        );
    let mut fixture = Fixture::new(local).await;
    let a = life_a.clone();
    let b = life_b.clone();
    fixture
        .ingress
        .handle()
        .set_run_resolver(Arc::new(move |id| {
            if id == run_a {
                Some(a.clone())
            } else if id == run_b {
                Some(b.clone())
            } else {
                None
            }
        }));
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let app_binding = fixture.request();
    let mut binding_a = app_binding.clone();
    binding_a.channel_id = Uuid::new_v4();
    binding_a.scope = Scope::Run { run_id: run_a };
    let mut binding_b = app_binding.clone();
    binding_b.channel_id = Uuid::new_v4();
    binding_b.scope = Scope::Run { run_id: run_b };
    let mut app = fixture.channel(&mut control, &app_binding).await;
    let mut a = fixture.channel(&mut control, &binding_a).await;
    let mut b = fixture.channel(&mut control, &binding_b).await;
    // An application channel is never a back door into an exact Run scope.
    app.send(&Frame::Open {
        id: 1,
        kind: secure::Kind::Http,
        method: "GET".into(),
        path: path_a.clone(),
        headers: vec![],
        body_length: 0,
        receive_window: secure::WINDOW,
    })
    .await
    .unwrap();
    assert!(matches!(
        app.next().await.unwrap(),
        Frame::Reset { id: 1, .. }
    ));
    life_a.revoke();
    closed(&mut a).await;
    assert!(life_b.active());
    assert!(
        fixture
            .ingress
            .handle()
            .permission(app_binding.context.grant_id)
            .unwrap()
            .active()
    );
    app.request(2, "GET", "/workbench/v1/application", vec![], &[])
        .await
        .unwrap();
    let response = app.response(2, "GET").await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.bytes().await.is_ok());
    b.request(1, "GET", &path_b, vec![], &[]).await.unwrap();
    let response = b.response(1, "GET").await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.bytes().await.is_ok());
    binding_a.channel_id = Uuid::new_v4();
    server_json(
        &mut control,
        &ToGateway::OpenChannel {
            binding: binding_a.clone(),
        },
    )
    .await;
    timeout(Duration::from_secs(2), async {
        loop {
            if let Some(Ok(ServerMessage::Text(text))) = control.recv().await
                && matches!(serde_json::from_str::<ToRelay>(&text),Ok(ToRelay::ChannelFailed{channel_id,..}) if channel_id==binding_a.channel_id)
            {
                break;
            }
        }
    })
    .await
    .expect("retired run must reject a fresh encrypted channel");
    assert!(fixture.data.try_recv().is_err());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn grant_revocation_closes_all_its_channels_but_preserves_another_grant() {
    let local = Router::new().route(
        "/workbench/v1/application",
        get(|| async { axum::Json(serde_json::json!({"ok":true})) }),
    );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let binding = fixture.request();
    let mut sibling = binding.clone();
    sibling.channel_id = Uuid::new_v4();
    let mut first = fixture.channel(&mut control, &binding).await;
    let mut second = fixture.channel(&mut control, &sibling).await;
    let unrelated = fixture.request();
    let mut third = fixture.channel(&mut control, &unrelated).await;
    server_json(
        &mut control,
        &ToGateway::Revoke {
            session_id: binding.context.grant_id,
        },
    )
    .await;
    closed(&mut first).await;
    closed(&mut second).await;
    assert!(
        fixture
            .ingress
            .handle()
            .grant(binding.context.grant_id)
            .is_err()
    );
    third
        .request(1, "GET", "/workbench/v1/application", vec![], &[])
        .await
        .unwrap();
    let response = third.response(1, "GET").await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.bytes().await.is_ok());
    assert!(!task.is_finished());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn outbound_stream_uses_private_ingress_and_revoke_fences_session() {
    use crate::workbench::remote_access::Auth;
    let local=Router::new().route("/workbench/v1/application",get(|headers:Auth|async move {
        let owner=headers.remote.as_ref().expect("private ingress must inject trusted owner");
        assert!(!headers.contains_key("cookie"));assert!(!headers.contains_key("authorization"));
        assert!(!headers.contains_key("x-forwarded-host"));
        assert!(headers.remote_origin());
        ([(axum::http::header::SET_COOKIE,"private=never-forward")],axum::Json(serde_json::json!({"body":"synthetic-local-application","instanceId":owner.instance_id()})))
    }));
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let binding = fixture.request();
    let mut peer = fixture.channel(&mut control, &binding).await;
    peer.request(
        1,
        "GET",
        "/workbench/v1/application",
        vec![
            ("Cookie".into(), "hostile-owner".into()),
            ("Authorization".into(), "Bearer hostile".into()),
            ("X-Forwarded-Host".into(), "hostile.invalid".into()),
            ("Host".into(), "hostile.invalid".into()),
            ("Origin".into(), "https://hostile.invalid".into()),
        ],
        &[],
    )
    .await
    .unwrap();
    let response = peer.response(1, "GET").await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        !response.headers().contains_key("set-cookie"),
        "private ingress cookies must never enter the encrypted business response"
    );
    let body: serde_json::Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(body["body"], "synthetic-local-application");
    assert_eq!(body["instanceId"], fixture.instance.to_string());
    assert!(
        fixture
            .ingress
            .handle()
            .grant(binding.context.grant_id)
            .is_ok()
    );
    server_json(
        &mut control,
        &ToGateway::Revoke {
            session_id: binding.context.grant_id,
        },
    )
    .await;
    timeout(Duration::from_secs(2), async {
        while fixture
            .ingress
            .handle()
            .grant(binding.context.grant_id)
            .is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    assert!(fixture.ingress.handle().grant(Uuid::new_v4()).is_err());
}

#[tokio::test]
async fn terminal_preserves_text_ack_and_binary_without_replay() {
    let run_id = Uuid::new_v4();
    let path = format!("/workbench/v1/runs/{run_id}/terminal");
    let (seen, mut received) = mpsc::unbounded_channel();
    let local = Router::new().route(
        &path,
        get(move |ws: WebSocketUpgrade| {
            let seen = seen.clone();
            async move {
                ws.on_upgrade(move |mut socket| async move {
                    socket
                        .send(ServerMessage::Text("s".repeat(160 * 1024).into()))
                        .await
                        .unwrap();
                    while let Some(Ok(message)) = socket.recv().await {
                        if matches!(message, ServerMessage::Text(_) | ServerMessage::Binary(_)) {
                            let bytes = match &message {
                                ServerMessage::Text(t) => t.as_bytes().to_vec(),
                                ServerMessage::Binary(b) => b.to_vec(),
                                _ => unreachable!(),
                            };
                            let _ = seen.send(bytes);
                            if socket.send(message).await.is_err() {
                                break;
                            }
                        }
                    }
                })
            }
        }),
    );
    let mut fixture = Fixture::new(local).await;
    let life = crate::workbench::permission::Permission::new();
    fixture
        .ingress
        .handle()
        .set_run_resolver(Arc::new(move |id| (id == run_id).then(|| life.clone())));
    let task = fixture.spawn(true);
    let mut control = fixture.handshake().await;
    let mut binding = fixture.request();
    binding.scope = Scope::Run { run_id };
    let mut terminal = fixture
        .channel(&mut control, &binding)
        .await
        .terminal(1, &path)
        .await
        .unwrap();
    let Message::Text(snapshot) = terminal.next().await.unwrap().unwrap() else {
        panic!("missing native terminal snapshot")
    };
    assert_eq!(snapshot.len(), 160 * 1024);
    assert!(snapshot.as_str().bytes().all(|byte| byte == b's'));
    let ack = r#"{"type":"ack","sequence":7}"#;
    terminal.send(Message::Text(ack.into())).await.unwrap();
    assert_eq!(
        terminal.next().await.unwrap().unwrap(),
        Message::Text(ack.into())
    );
    let binary = vec![0, 1, 2, 255];
    terminal
        .send(Message::Binary(binary.clone().into()))
        .await
        .unwrap();
    assert_eq!(
        terminal.next().await.unwrap().unwrap(),
        Message::Binary(binary.clone().into())
    );
    assert_eq!(received.recv().await.unwrap(), ack.as_bytes());
    assert_eq!(received.recv().await.unwrap(), binary);
    control.send(ServerMessage::Close(None)).await.unwrap();
    fixture.welcome.connection_epoch = Uuid::new_v4();
    let _fresh_control = fixture.handshake().await;
    assert!(
        timeout(Duration::from_secs(2), terminal.next())
            .await
            .expect("old encrypted terminal must close")
            .is_none_or(|message| message.is_err())
    );
    assert!(
        timeout(Duration::from_millis(150), fixture.data.recv())
            .await
            .is_err(),
        "new epoch must not recreate the terminal"
    );
    assert!(
        timeout(Duration::from_millis(150), received.recv())
            .await
            .is_err(),
        "text acknowledgements and binary input must never replay"
    );
    task.abort();
    let _ = task.await;
}
