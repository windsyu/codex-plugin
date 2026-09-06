use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
use tokio_tungstenite::{accept_async, client_async};

use super::*;

struct Fixture {
    _temp: TempDir,
    proxy: ProxyHandle,
    sink: Arc<RecordingEventSink>,
    upstream: JoinHandle<Result<tokio_tungstenite::WebSocketStream<UnixStream>>>,
}

impl Fixture {
    async fn start() -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix("codex-proxy-test-")
            .tempdir_in("/private/tmp")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let upstream_path = temp.path().join("upstream.sock");
        let proxy_path = temp.path().join("proxy.sock");
        let listener = UnixListener::bind(&upstream_path)
            .with_context(|| format!("bind fake upstream {}", upstream_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&upstream_path, std::fs::Permissions::from_mode(0o600))?;
        }
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            Ok(accept_async(stream).await?)
        });
        let sink = Arc::new(RecordingEventSink::default());
        let proxy = ProxyServer::bind(ProxyConfig {
            worker_id: "worker-fixture".into(),
            source_id: "source-fixture".into(),
            store_source_id: "store-fixture".into(),
            source_epoch: "source-epoch-fixture".into(),
            upstream_socket: upstream_path,
            private_socket: proxy_path,
            event_sink: sink.clone(),
        })
        .context("bind fixture proxy")?;
        proxy.authorize_downstream_pid(std::process::id())?;
        Ok(Self {
            _temp: temp,
            proxy,
            sink,
            upstream,
        })
    }

    async fn connect_tui(&self) -> Result<tokio_tungstenite::WebSocketStream<UnixStream>> {
        let stream = UnixStream::connect(self.proxy.socket_path()).await?;
        Ok(client_async("ws://localhost/", stream).await?.0)
    }

    async fn upstream(
        self,
    ) -> Result<(
        tokio_tungstenite::WebSocketStream<UnixStream>,
        ProxyHandle,
        Arc<RecordingEventSink>,
    )> {
        Ok((
            self.upstream.await.context("fake App Server panicked")??,
            self.proxy,
            self.sink,
        ))
    }
}

async fn text(websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>) -> Result<Value> {
    let message = websocket.next().await.context("WebSocket closed")??;
    let Message::Text(text) = message else {
        anyhow::bail!("expected text WebSocket message");
    };
    Ok(serde_json::from_str(&text)?)
}

#[tokio::test]
async fn recorded_0_146_1_fixture_is_semantically_transparent_for_new_and_resume() -> Result<()> {
    let fixture = Fixture::start().await?;
    fixture.proxy.set_input_owner(Some(ProxyOwner {
        owner_type: "terminal".into(),
        owner_id: "attachment-fixture".into(),
        principal_id: "local_bearer".into(),
        input_lease_id: Some("lease-fixture".into()),
    }));
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    let mut mapped_ids = HashMap::<String, Value>::new();
    let mut methods = Vec::new();

    for line in include_str!("../../../fixtures/app-server-session-proxy-0.146.1.jsonl").lines() {
        let record: Value = serde_json::from_str(line)?;
        let direction = record["direction"]
            .as_str()
            .context("fixture direction missing")?;
        let mut expected = record["message"].clone();
        if let Some(method) = expected.get("method").and_then(Value::as_str) {
            methods.push(method.to_string());
        }
        match direction {
            "tui_to_upstream" => {
                tui.send(Message::Text(expected.to_string().into())).await?;
                let actual = text(&mut upstream).await?;
                if expected.get("method").is_some() && expected.get("id").is_some() {
                    let original_id = expected["id"].clone();
                    assert_ne!(actual["id"], original_id);
                    mapped_ids.insert(serde_json::to_string(&original_id)?, actual["id"].clone());
                    expected["id"] = actual["id"].clone();
                }
                assert_eq!(actual, expected);
            }
            "upstream_to_tui" => {
                if expected.get("method").is_none() {
                    let original_id = expected["id"].clone();
                    expected["id"] = mapped_ids
                        .get(&serde_json::to_string(&original_id)?)
                        .context("fixture response has no mapped request")?
                        .clone();
                }
                upstream
                    .send(Message::Text(expected.to_string().into()))
                    .await?;
                let actual = text(&mut tui).await?;
                if expected.get("method").is_none() {
                    let mapped_id = expected["id"].clone();
                    let original_id = mapped_ids
                        .iter()
                        .find_map(|(original, mapped)| {
                            (mapped == &mapped_id).then(|| serde_json::from_str(original))
                        })
                        .context("fixture response mapping disappeared")??;
                    expected["id"] = original_id;
                }
                assert_eq!(actual, expected);
            }
            other => anyhow::bail!("unsupported fixture direction {other}"),
        }
    }

    assert!(methods.iter().any(|method| method == "initialize"));
    assert!(methods.iter().any(|method| method == "thread/start"));
    assert!(methods.iter().any(|method| method == "thread/resume"));
    assert!(methods.iter().any(|method| method == "future/tuiMutation"));
    assert_eq!(
        sink.steps()
            .iter()
            .filter(|step| matches!(step, RecordedStep::Raw(_, _)))
            .count(),
        13
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn initialize_notification_and_response_are_raw_first_and_id_mapped() -> Result<()> {
    let fixture = Fixture::start().await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(fixture.proxy.socket_path())?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut tui = fixture.connect_tui().await?;
    let mut second_tui = fixture.connect_tui().await?;
    let rejection = text(&mut second_tui).await?;
    assert_eq!(rejection["params"]["code"], "PROXY_ALREADY_ATTACHED");
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    let initialize = json!({
        "method":"initialize",
        "id":7,
        "params":{
            "clientInfo":{"name":"codex_tui","title":"Codex TUI","version":"fixture"},
            "capabilities":{"experimentalApi":true}
        }
    });
    tui.send(Message::Text(initialize.to_string().into()))
        .await?;
    let forwarded = text(&mut upstream).await?;
    assert_eq!(forwarded["params"], initialize["params"]);
    assert_ne!(forwarded["id"], initialize["id"]);
    upstream
        .send(Message::Text(
            json!({"method":"thread/started","params":{"thread":{"id":"thread-1"}}})
                .to_string()
                .into(),
        ))
        .await?;
    upstream
        .send(Message::Text(
            json!({"id":forwarded["id"],"result":{"codexHome":"/fixture"}})
                .to_string()
                .into(),
        ))
        .await?;
    assert_eq!(text(&mut tui).await?["method"], "thread/started");
    assert_eq!(text(&mut tui).await?["id"], 7);
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Raw(2, ProxyDirection::UpstreamToTui),
            RecordedStep::Raw(3, ProxyDirection::UpstreamToTui),
        ]
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn fragmented_frames_and_out_of_order_responses_preserve_message_and_id_order() -> Result<()>
{
    let fixture = Fixture::start().await?;
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;

    let first =
        json!({"method":"thread/read","id":"first","params":{"threadId":"thread-1"}}).to_string();
    let split = first.len() / 2;
    tui.send(Message::Frame(Frame::message(
        first.as_bytes()[..split].to_vec(),
        OpCode::Data(Data::Text),
        false,
    )))
    .await?;
    tui.send(Message::Frame(Frame::message(
        first.as_bytes()[split..].to_vec(),
        OpCode::Data(Data::Continue),
        true,
    )))
    .await?;
    tui.send(Message::Text(
        json!({"method":"thread/read","id":2,"params":{"threadId":"thread-2"}})
            .to_string()
            .into(),
    ))
    .await?;

    let first_upstream = text(&mut upstream).await?;
    let second_upstream = text(&mut upstream).await?;
    assert_ne!(first_upstream["id"], "first");
    assert_ne!(second_upstream["id"], 2);

    upstream
        .send(Message::Text(
            json!({"id":second_upstream["id"],"result":{"thread":{"id":"thread-2"}}})
                .to_string()
                .into(),
        ))
        .await?;
    upstream
        .send(Message::Text(
            json!({"id":first_upstream["id"],"result":{"thread":{"id":"thread-1"}}})
                .to_string()
                .into(),
        ))
        .await?;
    assert_eq!(text(&mut tui).await?["id"], 2);
    assert_eq!(text(&mut tui).await?["id"], "first");
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Raw(2, ProxyDirection::TuiToUpstream),
            RecordedStep::Raw(3, ProxyDirection::UpstreamToTui),
            RecordedStep::Raw(4, ProxyDirection::UpstreamToTui),
        ]
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn burst_backpressure_keeps_proxy_sequence_bounded_and_lossless() -> Result<()> {
    const ENVELOPES: u64 = 256;
    let fixture = Fixture::start().await?;
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;

    let sender = tokio::spawn(async move {
        for index in 0..ENVELOPES {
            upstream
                .send(Message::Text(
                    json!({"method":"item/agentMessage/delta","params":{"index":index,"chunk":"x".repeat(4096)}})
                        .to_string()
                        .into(),
                ))
                .await?;
        }
        Result::<_>::Ok(upstream)
    });
    for index in 0..ENVELOPES {
        let envelope = text(&mut tui).await?;
        assert_eq!(envelope["params"]["index"], index);
    }
    let mut upstream = sender.await??;
    assert_eq!(
        sink.steps()
            .iter()
            .filter(|step| matches!(step, RecordedStep::Raw(_, _)))
            .count(),
        ENVELOPES as usize
    );
    assert_eq!(
        sink.steps().last(),
        Some(&RecordedStep::Raw(ENVELOPES, ProxyDirection::UpstreamToTui))
    );
    upstream.close(None).await?;
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn mutation_is_raw_and_audited_before_upstream_write() -> Result<()> {
    let fixture = Fixture::start().await?;
    fixture.proxy.set_input_owner(Some(ProxyOwner {
        owner_type: "terminal".into(),
        owner_id: "attachment-1".into(),
        principal_id: "local_bearer".into(),
        input_lease_id: Some("lease-1".into()),
    }));
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    tui.send(Message::Text(
        json!({"method":"turn/start","id":"tui-1","params":{"threadId":"thread-1","input":[]}})
            .to_string()
            .into(),
    ))
    .await?;
    let forwarded = text(&mut upstream).await?;
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Prepared(1, ProtocolClassification::KnownMutation),
            RecordedStep::Written("command-1".into()),
        ]
    );
    upstream
        .send(Message::Text(
            json!({"id":forwarded["id"],"result":{"turn":{"id":"turn-1"}}})
                .to_string()
                .into(),
        ))
        .await?;
    assert_eq!(text(&mut tui).await?["id"], "tui-1");
    assert_eq!(
        sink.steps().last(),
        Some(&RecordedStep::Responded("command-1".into()))
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn raw_commit_failure_and_unattributed_unknown_never_write_upstream() -> Result<()> {
    let fixture = Fixture::start().await?;
    fixture.sink.fail_raw_at(1);
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, _) = fixture.upstream().await?;
    tui.send(Message::Text(
        json!({"method":"initialize","id":1,"params":{}})
            .to_string()
            .into(),
    ))
    .await?;
    let upstream_observation =
        tokio::time::timeout(Duration::from_millis(100), upstream.next()).await;
    assert!(
        !matches!(upstream_observation, Ok(Some(Ok(Message::Text(_))))),
        "raw commit failure must not forward a text envelope upstream"
    );
    assert!(proxy.shutdown().await.is_err());

    let fixture = Fixture::start().await?;
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    tui.send(Message::Text(
        json!({"method":"future/write","id":null,"params":{"value":1}})
            .to_string()
            .into(),
    ))
    .await?;
    let rejected = text(&mut tui).await?;
    assert_eq!(rejected["id"], Value::Null);
    assert_eq!(rejected["error"]["message"], "INPUT_ATTRIBUTION_UNKNOWN");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), upstream.next())
            .await
            .is_err()
    );
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Anomaly(1, "INPUT_ATTRIBUTION_UNKNOWN"),
        ]
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn tui_notifications_are_read_only_allowlisted_or_owner_audited() -> Result<()> {
    let fixture = Fixture::start().await?;
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;

    tui.send(Message::Text(
        json!({"method":"initialized"}).to_string().into(),
    ))
    .await?;
    assert_eq!(text(&mut upstream).await?["method"], "initialized");

    tui.send(Message::Text(
        json!({"method":"future/unattributedNotification","params":{"value":1}})
            .to_string()
            .into(),
    ))
    .await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), upstream.next())
            .await
            .is_err(),
        "an unattributed possible-mutation notification must not reach upstream"
    );
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Raw(2, ProxyDirection::TuiToUpstream),
            RecordedStep::Anomaly(2, "INPUT_ATTRIBUTION_UNKNOWN"),
        ]
    );

    proxy.set_input_owner(Some(ProxyOwner {
        owner_type: "terminal".into(),
        owner_id: "attachment-notification".into(),
        principal_id: "local_bearer".into(),
        input_lease_id: Some("lease-notification".into()),
    }));
    tui.send(Message::Text(
        json!({"method":"future/ownedNotification","params":{"value":2}})
            .to_string()
            .into(),
    ))
    .await?;
    assert_eq!(
        text(&mut upstream).await?["method"],
        "future/ownedNotification"
    );
    upstream.close(None).await?;
    drop(upstream);
    tokio::time::sleep(Duration::from_millis(50)).await;
    proxy.shutdown().await?;
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Raw(2, ProxyDirection::TuiToUpstream),
            RecordedStep::Anomaly(2, "INPUT_ATTRIBUTION_UNKNOWN"),
            RecordedStep::Raw(3, ProxyDirection::TuiToUpstream),
            RecordedStep::Prepared(3, ProtocolClassification::UnknownPossibleMutation),
            RecordedStep::Written("command-3".into()),
            RecordedStep::OutcomeUnknown("command-3".into()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn recoverable_decode_errors_are_committed_and_do_not_close_the_connection() -> Result<()> {
    let fixture = Fixture::start().await?;
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;

    tui.send(Message::Text("{not-json".into())).await?;
    assert_eq!(text(&mut tui).await?["error"]["message"], "INVALID_JSON");
    tui.send(Message::Text(
        json!({"method":"initialize","id":1,"params":{}})
            .to_string()
            .into(),
    ))
    .await?;
    let initialize = text(&mut upstream).await?;
    assert_eq!(initialize["method"], "initialize");

    upstream.send(Message::Text("[]".into())).await?;
    upstream
        .send(Message::Text(
            json!({"method":"thread/started","params":{"thread":{"id":"thread-1"}}})
                .to_string()
                .into(),
        ))
        .await?;
    assert_eq!(text(&mut tui).await?["method"], "thread/started");
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::DecodeError(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Anomaly(1, "INVALID_JSON"),
            RecordedStep::Raw(2, ProxyDirection::TuiToUpstream),
            RecordedStep::Raw(3, ProxyDirection::UpstreamToTui),
            RecordedStep::Anomaly(3, "INVALID_ENVELOPE"),
            RecordedStep::Raw(4, ProxyDirection::UpstreamToTui),
        ]
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn private_socket_is_removed_when_upstream_validation_fails() -> Result<()> {
    let temp = tempfile::Builder::new()
        .prefix("codex-proxy-cleanup-")
        .tempdir_in("/private/tmp")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let invalid_upstream = temp.path().join("not-a-socket");
    std::fs::write(&invalid_upstream, b"fixture")?;
    let proxy_path = temp.path().join("proxy.sock");
    let proxy = ProxyServer::bind(ProxyConfig {
        worker_id: "worker-cleanup".into(),
        source_id: "source-cleanup".into(),
        store_source_id: "store-cleanup".into(),
        source_epoch: "epoch-cleanup".into(),
        upstream_socket: invalid_upstream,
        private_socket: proxy_path.clone(),
        event_sink: Arc::new(RecordingEventSink::default()),
    })?;
    proxy.authorize_downstream_pid(std::process::id())?;
    let stream = UnixStream::connect(proxy.socket_path()).await?;
    let _tui = client_async("ws://localhost/", stream).await?.0;
    assert!(proxy.shutdown().await.is_err());
    assert!(!proxy_path.exists());
    Ok(())
}

#[tokio::test]
async fn dropping_proxy_handle_requests_shutdown_and_removes_private_socket() -> Result<()> {
    let temp = tempfile::Builder::new()
        .prefix("codex-proxy-drop-cleanup-")
        .tempdir_in("/private/tmp")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let upstream_path = temp.path().join("upstream.sock");
    let _upstream = UnixListener::bind(&upstream_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&upstream_path, std::fs::Permissions::from_mode(0o600))?;
    }
    let proxy_path = temp.path().join("proxy.sock");
    let proxy = ProxyServer::bind(ProxyConfig {
        worker_id: "worker-drop-cleanup".into(),
        source_id: "source-drop-cleanup".into(),
        store_source_id: "store-drop-cleanup".into(),
        source_epoch: "epoch-drop-cleanup".into(),
        upstream_socket: upstream_path,
        private_socket: proxy_path.clone(),
        event_sink: Arc::new(RecordingEventSink::default()),
    })?;
    assert!(proxy_path.exists());
    drop(proxy);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while proxy_path.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!proxy_path.exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn private_proxy_rejects_symlink_parent_and_non_worker_peer_pid() -> Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temp = tempfile::Builder::new()
        .prefix("codex-proxy-auth-")
        .tempdir_in("/private/tmp")?;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    let real_parent = temp.path().join("real");
    std::fs::create_dir(&real_parent)?;
    std::fs::set_permissions(&real_parent, std::fs::Permissions::from_mode(0o700))?;
    let linked_parent = temp.path().join("linked");
    symlink(&real_parent, &linked_parent)?;
    assert!(validate_private_socket_parent(&linked_parent.join("proxy.sock")).is_err());

    let upstream_path = temp.path().join("upstream.sock");
    let listener = UnixListener::bind(&upstream_path)?;
    std::fs::set_permissions(&upstream_path, std::fs::Permissions::from_mode(0o600))?;
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        Result::<_>::Ok(accept_async(stream).await?)
    });
    let proxy = ProxyServer::bind(ProxyConfig {
        worker_id: "worker-auth".into(),
        source_id: "source-auth".into(),
        store_source_id: "store-auth".into(),
        source_epoch: "epoch-auth".into(),
        upstream_socket: upstream_path,
        private_socket: real_parent.join("proxy.sock"),
        event_sink: Arc::new(RecordingEventSink::default()),
    })?;
    proxy.authorize_downstream_pid(std::process::id().saturating_add(1))?;
    assert!(
        proxy.authorize_downstream_pid(std::process::id()).is_err(),
        "downstream PID authorization must be one-use"
    );
    let stream = UnixStream::connect(proxy.socket_path()).await?;
    let attempt = client_async("ws://localhost/", stream).await;
    assert!(attempt.is_err());
    let failure = proxy
        .shutdown()
        .await
        .expect_err("wrong worker PID must fail the private proxy");
    assert!(
        failure
            .to_string()
            .contains("authorized Session Worker child")
    );
    upstream.abort();
    Ok(())
}

#[tokio::test]
async fn tui_disconnect_after_mutation_write_marks_outcome_unknown() -> Result<()> {
    let fixture = Fixture::start().await?;
    fixture.proxy.set_input_owner(Some(ProxyOwner {
        owner_type: "terminal".into(),
        owner_id: "attachment-disconnect".into(),
        principal_id: "local_bearer".into(),
        input_lease_id: Some("lease-disconnect".into()),
    }));
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    tui.send(Message::Text(
        json!({"method":"turn/start","id":1,"params":{"threadId":"thread-1","input":[]}})
            .to_string()
            .into(),
    ))
    .await?;
    let _ = text(&mut upstream).await?;
    tui.close(None).await?;
    drop(tui);
    proxy.shutdown().await?;
    assert!(
        sink.steps()
            .contains(&RecordedStep::OutcomeUnknown("command-1".into()))
    );
    Ok(())
}

#[tokio::test]
async fn closed_interrupt_control_is_raw_first_audited_and_not_reflected_as_a_tui_response()
-> Result<()> {
    let fixture = Fixture::start().await?;
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    let control = proxy.control_handle();
    let interrupt = tokio::spawn(async move {
        control
            .interrupt(
                "thread-1".into(),
                "turn-1".into(),
                MutationBoundary {
                    command_id: "interrupt-command".into(),
                },
            )
            .await
    });
    let request = text(&mut upstream).await?;
    assert_eq!(request["method"], "turn/interrupt");
    assert_eq!(request["params"]["threadId"], "thread-1");
    assert_eq!(request["params"]["turnId"], "turn-1");
    assert!(
        request["id"]
            .as_str()
            .unwrap()
            .starts_with("gateway:control:")
    );
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Written("interrupt-command".into()),
        ]
    );
    upstream
        .send(Message::Text(
            json!({"id":request["id"],"result":{}}).to_string().into(),
        ))
        .await?;
    assert_eq!(interrupt.await??["result"], json!({}));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), tui.next())
            .await
            .is_err(),
        "worker-control responses must not enter the TUI request namespace"
    );
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::TuiToUpstream),
            RecordedStep::Written("interrupt-command".into()),
            RecordedStep::Raw(2, ProxyDirection::UpstreamToTui),
            RecordedStep::Responded("interrupt-command".into()),
        ]
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn server_request_ids_are_type_preserving_and_disconnect_is_outcome_unknown() -> Result<()> {
    let fixture = Fixture::start().await?;
    fixture.proxy.set_input_owner(Some(ProxyOwner {
        owner_type: "terminal".into(),
        owner_id: "attachment-2".into(),
        principal_id: "local_cookie".into(),
        input_lease_id: Some("lease-2".into()),
    }));
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;

    for request_id in [json!(11), json!("eleven"), Value::Null] {
        upstream
            .send(Message::Text(
                json!({"method":"item/tool/requestUserInput","id":request_id,"params":{"threadId":"thread-1"}})
                    .to_string()
                    .into(),
            ))
            .await?;
        let request = text(&mut tui).await?;
        assert_eq!(request["id"], request_id);
        tui.send(Message::Text(
            json!({"id":request_id,"result":{"answers":{}}})
                .to_string()
                .into(),
        ))
        .await?;
        assert_eq!(text(&mut upstream).await?["id"], request_id);
    }

    tui.send(Message::Text(
        json!({"method":"turn/start","id":99,"params":{"threadId":"thread-1","input":[]}})
            .to_string()
            .into(),
    ))
    .await?;
    let _ = text(&mut upstream).await?;
    upstream.close(None).await?;
    drop(upstream);
    tokio::time::sleep(Duration::from_millis(50)).await;
    proxy.shutdown().await?;
    assert!(
        sink.steps()
            .contains(&RecordedStep::OutcomeUnknown("command-7".into()))
    );
    Ok(())
}

#[tokio::test]
async fn one_worker_upstream_disconnect_does_not_stop_a_peer_connection() -> Result<()> {
    let first = Fixture::start().await?;
    let second = Fixture::start().await?;
    for fixture in [&first, &second] {
        fixture.proxy.set_input_owner(Some(ProxyOwner {
            owner_type: "terminal".into(),
            owner_id: "attachment-isolation".into(),
            principal_id: "local_bearer".into(),
            input_lease_id: Some("lease-isolation".into()),
        }));
    }
    let mut first_tui = first.connect_tui().await?;
    let mut second_tui = second.connect_tui().await?;
    let (mut first_upstream, first_proxy, first_sink) = first.upstream().await?;
    let (mut second_upstream, second_proxy, second_sink) = second.upstream().await?;

    first_tui
        .send(Message::Text(
            json!({"method":"turn/start","id":"first","params":{"threadId":"thread-first","input":[]}})
                .to_string()
                .into(),
        ))
        .await?;
    let _ = text(&mut first_upstream).await?;
    first_upstream.close(None).await?;
    drop(first_upstream);
    tokio::time::sleep(Duration::from_millis(50)).await;
    first_proxy.shutdown().await?;
    assert!(
        first_sink
            .steps()
            .contains(&RecordedStep::OutcomeUnknown("command-1".into()))
    );

    second_tui
        .send(Message::Text(
            json!({"method":"turn/start","id":"second","params":{"threadId":"thread-second","input":[]}})
                .to_string()
                .into(),
        ))
        .await?;
    let forwarded = text(&mut second_upstream).await?;
    second_upstream
        .send(Message::Text(
            json!({"id":forwarded["id"],"result":{"turn":{"id":"turn-second"}}})
                .to_string()
                .into(),
        ))
        .await?;
    assert_eq!(text(&mut second_tui).await?["id"], "second");
    assert_eq!(
        second_sink.steps().last(),
        Some(&RecordedStep::Responded("command-1".into()))
    );
    second_proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn channel_owned_server_request_is_withheld_validated_and_resolved_once() -> Result<()> {
    let fixture = Fixture::start().await?;
    fixture
        .sink
        .set_server_request_route(ServerRequestRoute::Channel {
            owner_type: "channel".into(),
            owner_id: "fake:conversation".into(),
            principal_id: "channel:fixture".into(),
        });
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    upstream
        .send(Message::Text(
            json!({
                "method":"item/tool/requestUserInput",
                "id":"request-1",
                "params":{
                    "threadId":"thread-1",
                    "turnId":"turn-1",
                    "questions":[{"id":"q1","options":[{"label":"yes"}]}]
                }
            })
            .to_string()
            .into(),
        ))
        .await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), tui.next())
            .await
            .is_err(),
        "channel-owned callback must not be forwarded to the TUI"
    );
    let request_id = serde_json::to_string(&json!("request-1"))?;
    let response = proxy
        .control_handle()
        .prepare_request_action(
            request_id.clone(),
            PendingRequestAction::UserInput {
                answers: BTreeMap::from([("q1".into(), vec!["yes".into()])]),
            },
        )
        .await?;
    assert_eq!(response, json!({"answers":{"q1":{"answers":["yes"]}}}));
    proxy
        .control_handle()
        .resolve_request(
            request_id.clone(),
            response,
            MutationBoundary {
                command_id: "request-command".into(),
            },
        )
        .await?;
    let resolved = text(&mut upstream).await?;
    assert_eq!(resolved["id"], "request-1");
    assert_eq!(
        resolved["result"]["answers"]["q1"]["answers"],
        json!(["yes"])
    );
    let duplicate = proxy
        .control_handle()
        .prepare_request_action(
            request_id,
            PendingRequestAction::UserInput {
                answers: BTreeMap::from([("q1".into(), vec!["yes".into()])]),
            },
        )
        .await
        .expect_err("resolved callback cannot be prepared twice");
    assert!(duplicate.to_string().contains("REQUEST_NOT_PENDING"));
    assert_eq!(
        sink.steps(),
        vec![
            RecordedStep::Raw(1, ProxyDirection::UpstreamToTui),
            RecordedStep::Raw(2, ProxyDirection::TuiToUpstream),
            RecordedStep::Written("request-command".into()),
            RecordedStep::Responded("request-command".into()),
        ]
    );
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn terminal_and_channel_routes_preserve_all_supported_interactive_request_semantics()
-> Result<()> {
    let cases = vec![
        (
            "item/commandExecution/requestApproval",
            json!({"threadId":"thread-1","turnId":"turn-1","availableDecisions":["accept","decline"]}),
            PendingRequestAction::Approval {
                decision: "accept".into(),
            },
            json!({"decision":"accept"}),
        ),
        (
            "item/fileChange/requestApproval",
            json!({"threadId":"thread-1","turnId":"turn-1","changes":[{"path":"fixture.txt"}]}),
            PendingRequestAction::Approval {
                decision: "decline".into(),
            },
            json!({"decision":"decline"}),
        ),
        (
            "item/permissions/requestApproval",
            json!({"threadId":"thread-1","turnId":"turn-1","permissions":{"network":true}}),
            PendingRequestAction::Permissions {
                grant: true,
                scope: "turn".into(),
                strict_auto_review: Some(false),
            },
            json!({"permissions":{"network":true},"scope":"turn","strictAutoReview":false}),
        ),
        (
            "item/tool/requestUserInput",
            json!({"threadId":"thread-1","turnId":"turn-1","questions":[{"id":"q1","options":[{"label":"yes"}]}]}),
            PendingRequestAction::UserInput {
                answers: BTreeMap::from([("q1".into(), vec!["yes".into()])]),
            },
            json!({"answers":{"q1":{"answers":["yes"]}}}),
        ),
        (
            "mcpServer/elicitation/request",
            json!({
                "threadId":"thread-1",
                "turnId":"turn-1",
                "mode":"form",
                "requestedSchema":{
                    "type":"object",
                    "properties":{"name":{"type":"string","minLength":1}},
                    "required":["name"]
                }
            }),
            PendingRequestAction::McpElicitation {
                action: "accept".into(),
                content: Some(json!({"name":"fixture"})),
            },
            json!({"action":"accept","content":{"name":"fixture"}}),
        ),
    ];

    for (index, (method, params, action, expected)) in cases.into_iter().enumerate() {
        let fixture = Fixture::start().await?;
        fixture
            .sink
            .set_server_request_route(ServerRequestRoute::Channel {
                owner_type: "channel".into(),
                owner_id: "fake:conversation".into(),
                principal_id: "channel:fixture".into(),
            });
        let mut tui = fixture.connect_tui().await?;
        let (mut upstream, proxy, _) = fixture.upstream().await?;
        let rpc_id = json!(format!("channel-{index}"));
        upstream
            .send(Message::Text(
                json!({"method":method,"id":rpc_id,"params":params})
                    .to_string()
                    .into(),
            ))
            .await?;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), tui.next())
                .await
                .is_err(),
            "{method} must stay withheld from the TUI while channel-owned"
        );
        let request_id = serde_json::to_string(&rpc_id)?;
        let prepared = proxy
            .control_handle()
            .prepare_request_action(request_id.clone(), action)
            .await?;
        assert_eq!(prepared, expected, "{method} response shape changed");
        proxy
            .control_handle()
            .resolve_request(
                request_id,
                prepared,
                MutationBoundary {
                    command_id: format!("channel-command-{index}"),
                },
            )
            .await?;
        let response = text(&mut upstream).await?;
        assert_eq!(response["id"], rpc_id);
        assert_eq!(response["result"], expected);

        upstream
            .send(Message::Text(
                json!({"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","requestMethod":method}})
                    .to_string()
                    .into(),
            ))
            .await?;
        assert_eq!(text(&mut tui).await?["params"]["requestMethod"], method);
        proxy.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn terminal_route_round_trips_each_supported_interactive_request_without_rewriting()
-> Result<()> {
    for (index, method) in [
        "item/commandExecution/requestApproval",
        "item/fileChange/requestApproval",
        "item/permissions/requestApproval",
        "item/tool/requestUserInput",
        "mcpServer/elicitation/request",
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = Fixture::start().await?;
        let mut tui = fixture.connect_tui().await?;
        let (mut upstream, proxy, _) = fixture.upstream().await?;
        let rpc_id = if index % 2 == 0 {
            json!(index)
        } else {
            json!(format!("terminal-{index}"))
        };
        let request = json!({
            "method":method,
            "id":rpc_id,
            "params":{"threadId":"thread-1","turnId":"turn-1","opaque":{"preserve":true}}
        });
        upstream
            .send(Message::Text(request.to_string().into()))
            .await?;
        assert_eq!(text(&mut tui).await?, request);
        let response = json!({"id":rpc_id,"result":{"opaqueAnswer":{"preserve":true}}});
        tui.send(Message::Text(response.to_string().into())).await?;
        assert_eq!(text(&mut upstream).await?, response);
        proxy.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn unsupported_channel_request_can_handoff_to_terminal_without_losing_callback() -> Result<()>
{
    let fixture = Fixture::start().await?;
    fixture
        .sink
        .set_server_request_route(ServerRequestRoute::Channel {
            owner_type: "channel".into(),
            owner_id: "fake:conversation".into(),
            principal_id: "channel:fixture".into(),
        });
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, _) = fixture.upstream().await?;
    let request = json!({
        "method":"future/interactiveRequest",
        "id":17,
        "params":{"threadId":"thread-1","futureField":{"preserve":true}},
        "futureTopLevel":{"preserve":true}
    });
    upstream
        .send(Message::Text(request.to_string().into()))
        .await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), tui.next())
            .await
            .is_err(),
        "channel-owned request must remain withheld before explicit handoff"
    );
    let request_id = serde_json::to_string(&json!(17))?;
    let handoff = proxy
        .control_handle()
        .handoff_request_to_terminal(request_id)
        .await?;
    assert_eq!(handoff["deliveredTo"], "terminal");
    assert_eq!(text(&mut tui).await?, request);

    tui.send(Message::Text(
        json!({"id":17,"result":{"futureAnswer":true}})
            .to_string()
            .into(),
    ))
    .await?;
    assert_eq!(text(&mut upstream).await?["id"], 17);
    proxy.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn unattributed_request_retains_callback_for_explicit_terminal_handoff() -> Result<()> {
    let fixture = Fixture::start().await?;
    fixture
        .sink
        .set_server_request_route(ServerRequestRoute::Unattributed);
    let mut tui = fixture.connect_tui().await?;
    let (mut upstream, proxy, sink) = fixture.upstream().await?;
    let request = json!({
        "method":"future/unattributedInteractiveRequest",
        "id":"unattributed-1",
        "params":{"threadId":"thread-unknown","futureField":{"preserve":true}}
    });
    upstream
        .send(Message::Text(request.to_string().into()))
        .await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), tui.next())
            .await
            .is_err()
    );
    let request_id = serde_json::to_string(&json!("unattributed-1"))?;
    let guessed = proxy
        .control_handle()
        .prepare_request_action(
            request_id.clone(),
            PendingRequestAction::Approval {
                decision: "accept".into(),
            },
        )
        .await
        .expect_err("unattributed callback cannot be answered as channel-owned");
    assert!(guessed.to_string().contains("REQUEST_OWNER_UNATTRIBUTED"));
    proxy
        .control_handle()
        .handoff_request_to_terminal(request_id)
        .await?;
    assert_eq!(text(&mut tui).await?, request);
    tui.send(Message::Text(
        json!({"id":"unattributed-1","result":{"handled":true}})
            .to_string()
            .into(),
    ))
    .await?;
    assert_eq!(text(&mut upstream).await?["result"]["handled"], true);
    assert!(
        sink.steps()
            .contains(&RecordedStep::Anomaly(1, "REQUEST_OWNER_UNATTRIBUTED"))
    );
    proxy.shutdown().await?;
    Ok(())
}
