use super::*;
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
                "/relay/v1/tunnel/streams/{id}",
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
    fn request(&self) -> Open {
        Open {
            stream_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            instance_id: self.instance,
            kind: StreamKind::Http,
            method: "GET".into(),
            path: "/workbench/v1/application".into(),
            headers: vec![],
        }
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
async fn closed(socket: &mut WebSocket) {
    timeout(Duration::from_secs(2), async {
        while let Some(Ok(message)) = socket.recv().await {
            if matches!(message, ServerMessage::Close(_)) {
                break;
            }
        }
    })
    .await
    .expect("old data socket must close");
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
            axum::response::Response::new(Body::from_stream(futures_util::stream::pending::<
                Result<Bytes, std::convert::Infallible>,
            >()))
        }),
    );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(false);
    let mut control = fixture.handshake().await;
    let request = fixture.request();
    server_json(
        &mut control,
        &ToGateway::Open {
            request: request.clone(),
        },
    )
    .await;
    let mut data = timeout(Duration::from_secs(2), fixture.data.recv())
        .await
        .unwrap()
        .unwrap();
    server_json(&mut data, &Data::RequestEnd).await;
    assert!(matches!(
        timeout(Duration::from_secs(2), data.recv()).await.unwrap(),
        Some(Ok(ServerMessage::Text(_)))
    ));
    // Move the lease deadline off the heartbeat tick. The old tick-only timeout
    // waits almost another whole renewal interval before fencing this connection.
    tokio::time::sleep(Duration::from_millis(150)).await;
    server_json(
        &mut control,
        &ToGateway::Renew {
            session_ids: vec![request.session_id],
        },
    )
    .await;
    assert!(
        !timeout(Duration::from_secs(protocol::LEASE_SECONDS + 1), task)
            .await
            .expect("lease expiry must not wait for the next heartbeat tick")
            .unwrap()
    );
    assert!(fixture.ingress.handle().grant(request.session_id).is_err());
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
                "accepted"
            }
        }),
    );
    let mut fixture = Fixture::new(local).await;
    let task = fixture.spawn(true);
    let mut control = fixture.handshake().await;
    let request = fixture.request();
    server_json(
        &mut control,
        &ToGateway::Open {
            request: request.clone(),
        },
    )
    .await;
    let mut data = timeout(Duration::from_secs(2), fixture.data.recv())
        .await
        .unwrap()
        .unwrap();
    let old_capability = fixture.ingress.handle().grant(request.session_id).unwrap();
    data.send(ServerMessage::Binary(
        b"never-completed-input".to_vec().into(),
    ))
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
    let fresh_capability = fixture.ingress.handle().grant(request.session_id).unwrap();
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
    assert!(fixture.ingress.handle().grant(request.session_id).is_err());
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
    let path = format!("/workbench/v1/runs/{}/terminal", Uuid::new_v4());
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
    let task = fixture.spawn(true);
    let mut control = fixture.handshake().await;
    let mut request = fixture.request();
    request.path = path;
    request.kind = StreamKind::WebSocket;
    request.headers = vec![("Origin".into(), fixture.welcome.gateway_origin.clone())];
    server_json(&mut control, &ToGateway::Open { request }).await;
    let mut data = timeout(Duration::from_secs(2), fixture.data.recv())
        .await
        .unwrap()
        .unwrap();
    let Some(Ok(ServerMessage::Text(ready))) =
        timeout(Duration::from_secs(2), data.recv()).await.unwrap()
    else {
        panic!("missing ready");
    };
    assert!(matches!(
        serde_json::from_str::<Data>(&ready).unwrap(),
        Data::WebSocketReady
    ));
    data.send(ServerMessage::Text("synthetic terminal input".into()))
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
    closed(&mut data).await;
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
        let local = Router::new().route(
            "/workbench/v1/application",
            get(|| async {
                axum::response::Response::new(Body::from_stream(futures_util::stream::pending::<
                    Result<Bytes, std::convert::Infallible>,
                >()))
            }),
        );
        let mut fixture = Fixture::new(local).await;
        let handle = fixture.ingress.handle();
        handle
            .begin(
                fixture.welcome.account_id,
                fixture.welcome.gateway_id,
                fixture.welcome.connection_epoch,
                fixture.welcome.gateway_origin.clone(),
            )
            .unwrap();
        let request = fixture.request();
        let capability = handle.grant(request.session_id).unwrap();
        let saved = fixture.saved.clone();
        let epoch = fixture.welcome.connection_epoch;
        let task =
            tokio::spawn(async move { stream(&saved, epoch, &handle, request, &capability).await });
        let mut data = timeout(Duration::from_secs(2), fixture.data.recv())
            .await
            .unwrap()
            .unwrap();
        server_json(&mut data, &Data::RequestEnd).await;
        let response = timeout(Duration::from_secs(2), data.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ServerMessage::Text(head) = response else {
            panic!("response head expected");
        };
        assert!(matches!(
            serde_json::from_str::<Data>(&head).unwrap(),
            Data::ResponseHead { status: 200, .. }
        ));
        if invalid_frame {
            server_json(&mut data, &Data::RequestEnd).await;
        } else {
            data.send(ServerMessage::Close(None)).await.unwrap();
        }
        assert!(
            timeout(Duration::from_secs(2), task)
                .await
                .expect("idle SSE must not retain disconnected consumer")
                .unwrap()
                .is_err()
        );
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
            fixture.ingress.handle().grant(request.session_id).unwrap();
            let awake = Instant::now();
            clock.set(failure);
            let message = if open {
                ToGateway::Open {
                    request: request.clone(),
                }
            } else {
                ToGateway::Renew {
                    session_ids: vec![request.session_id],
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
            assert!(fixture.ingress.handle().grant(request.session_id).is_err());
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
